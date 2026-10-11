//! Object-only decoding. A derived `Deserialize` for a struct, a struct variant or an internally
//! tagged enum also takes serde's positional sequence form (a tagged enum's with the tag first),
//! which `deny_unknown_fields` does not constrain; a type decoded from a JSON object must refuse it.

use std::{fmt, marker::PhantomData};

use serde::{
    Deserialize, Deserializer,
    de::{MapAccess, Visitor, value::MapAccessDeserializer},
};

/// Decodes `T` from a map only.
pub struct Object<T>(pub T);

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Object<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Members<T>(PhantomData<T>);

        impl<'de, T: Deserialize<'de>> Visitor<'de> for Members<T> {
            type Value = Object<T>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an object")
            }

            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Object<T>, A::Error> {
                T::deserialize(MapAccessDeserializer::new(map)).map(Object)
            }
        }

        deserializer.deserialize_map(Members(PhantomData))
    }
}

/// A `deserialize_with` decoding a field's `T` from a map only.
pub fn deserialize<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<T, D::Error> {
    Object::deserialize(deserializer).map(|Object(value)| value)
}

/// Makes each listed type decode from a map only, wherever it appears.
///
/// Each type derives `Serialize` and `Deserialize` with `#[serde(remote = "Self")]`, which turns the
/// derived impls into inherent `serialize` and `deserialize` functions; this macro implements the
/// traits over them, decoding through [`Object`]. Every other serde attribute behaves as derived.
#[macro_export]
macro_rules! object_only {
    ($($ty:ty),+ $(,)?) => {$(
        impl<'de> ::serde::Deserialize<'de> for $ty {
            fn deserialize<D: ::serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                struct Fields($ty);

                impl<'de> ::serde::Deserialize<'de> for Fields {
                    fn deserialize<D: ::serde::Deserializer<'de>>(
                        deserializer: D,
                    ) -> Result<Self, D::Error> {
                        <$ty>::deserialize(deserializer).map(Fields)
                    }
                }

                <$crate::serde_object::Object<Fields> as ::serde::Deserialize>::deserialize(
                    deserializer,
                )
                .map(|$crate::serde_object::Object(Fields(value))| value)
            }
        }

        impl ::serde::Serialize for $ty {
            fn serialize<S: ::serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                <$ty>::serialize(self, serializer)
            }
        }
    )+};
}
