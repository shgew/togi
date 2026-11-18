use std::io;
use std::sync::Arc;

use togi_machine::FrequencyEvent;
use tokio::sync::oneshot;

/// A request is acknowledged only after the caller's journal append is durable.
#[derive(Debug)]
pub(crate) struct Request {
    pub(crate) event: FrequencyEvent,
    pub(crate) reply: oneshot::Sender<io::Result<()>>,
}

#[derive(Debug, thiserror::Error)]
#[error("record controller frequency event")]
pub(crate) struct Failure(#[source] pub(crate) io::Error);

pub(crate) enum Message<T> {
    Frequency(Request),
    Finished(T),
}

pub(crate) enum Reply<T> {
    Direct(std::sync::mpsc::SyncSender<T>),
    Journaled(std::sync::mpsc::Sender<Message<T>>),
}

impl<T> From<std::sync::mpsc::SyncSender<T>> for Reply<T> {
    fn from(sender: std::sync::mpsc::SyncSender<T>) -> Self {
        Self::Direct(sender)
    }
}

impl<T> Reply<T> {
    pub(crate) fn send(&self, value: T) -> Result<(), std::sync::mpsc::SendError<T>> {
        match self {
            Self::Direct(sender) => sender.send(value),
            Self::Journaled(sender) => {
                sender
                    .send(Message::Finished(value))
                    .map_err(|error| match error.0 {
                        Message::Finished(value) => std::sync::mpsc::SendError(value),
                        Message::Frequency(_) => unreachable!("only a completion was sent"),
                    })
            }
        }
    }
}

#[derive(Clone)]
pub(crate) struct Journal(Arc<dyn Fn(Request) -> io::Result<()> + Send + Sync>);

impl Journal {
    pub(crate) fn new(send: impl Fn(Request) -> io::Result<()> + Send + Sync + 'static) -> Self {
        Self(Arc::new(send))
    }

    pub(crate) fn channel<T: Send + 'static>()
    -> (Self, Reply<T>, std::sync::mpsc::Receiver<Message<T>>) {
        let (sender, receiver) = std::sync::mpsc::channel();
        let events = sender.clone();
        let journal = Self::new(move |request| {
            events
                .send(Message::Frequency(request))
                .map_err(|_| io::Error::other("frequency journal receiver ended"))
        });
        (journal, Reply::Journaled(sender), receiver)
    }
    pub(crate) async fn record(&self, event: FrequencyEvent) -> io::Result<()> {
        let (reply, answer) = oneshot::channel();
        let recorded = async {
            (self.0)(Request { event, reply })?;
            answer
                .await
                .map_err(|_| io::Error::other("frequency journal acknowledgement lost"))?
        }
        .await;
        recorded.map_err(|error| io::Error::other(Failure(error)))
    }

    #[cfg(test)]
    pub(crate) fn recorded() -> Self {
        Self::new(|request| {
            let _ = request.reply.send(Ok(()));
            Ok(())
        })
    }
}
