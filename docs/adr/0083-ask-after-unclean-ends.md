# Ask after unclean daily ends

Status: accepted for the Ruleset 13 stack.

A missing clean-stop marker is evidence of an unclean end, not proof that Curve Optimizer caused it. The owner chose to gather reset/kernel evidence and ask what happened, without counting orderly shutdowns as crashes ([#1214](https://github.com/shgew/togi/issues/1214), [#1215 Q15](https://github.com/shgew/togi/issues/1215)). [Daily events](../spec/tuner.md#daily-events) own classification, provisional retreat, profile vetoes, hold resets, unexpired escalation and the promotion block; [the journal](../spec/journal.md#day-observation) owns revisable raw answers.

Blaming the most recently changed CCD is rejected: an older CCD can survive a hold by chance. Permanently charging every unexplained end is also rejected because background/graphics failures would consume depth without CO evidence. A whole-profile veto preserves the contradiction while an answer can reclassify it; independent machine-check evidence cannot disappear merely because someone chose a non-CPU answer.

The ban on re-entering a vetoed profile concerns **day use**. Controlled-night reproduction is a separately budgeted exception, earns no qualification and a clean result never clears the veto. This reconciles the wording of Q15 with its explicit exact-profile reproduction requirement rather than reopening either decision.
