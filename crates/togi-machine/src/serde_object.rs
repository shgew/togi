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

/// Defines structs and struct-variant enums that decode from a map only.
///
/// Write each definition as usual, deriving `Serialize` but not `Deserialize`, inside the macro. It
/// emits the definition unchanged and implements `Deserialize` through [`Object`] over a private
/// mirror that derives `Deserialize` with the same `serde` attributes, so the type is object-only
/// wherever it appears and no inherent or trait path decodes it positionally. Fields and variants
/// carry their `serde` attributes (`default`, `rename`, `skip_serializing_if`) as in a derive; other
/// container attributes stay on the definition.
///
/// A struct has no inherent `deserialize`, so only the trait's object-only impl decodes it:
///
/// ```compile_fail,E0599
/// let mut input = serde_json::Deserializer::from_str("[1, 0, []]");
/// let _ = togi_machine::CoreInfo::deserialize(&mut input);
/// ```
#[macro_export]
macro_rules! object_only {
    (@attrs [$($def:tt)*] [$($mirror:tt)*] #[serde $args:tt] $($rest:tt)+) => {
        $crate::object_only!(@attrs [$($def)* #[serde $args]] [$($mirror)* #[serde $args]] $($rest)+);
    };
    (@attrs [$($def:tt)*] [$($mirror:tt)*] #[$other:meta] $($rest:tt)+) => {
        $crate::object_only!(@attrs [$($def)* #[$other]] [$($mirror)*] $($rest)+);
    };
    (
        @attrs [$($def:tt)*] [$($mirror:tt)*]
        $vis:vis struct $name:ident {
            $($(#[$field_meta:meta])* $field_vis:vis $field:ident : $ty:ty),* $(,)?
        }
        $($rest:tt)*
    ) => {
        $($def)*
        $vis struct $name {
            $($(#[$field_meta])* $field_vis $field: $ty),*
        }

        const _: () = {
            #[derive(::serde::Deserialize)]
            $($mirror)*
            struct Mirror {
                $($(#[$field_meta])* $field: $ty),*
            }

            impl<'de> ::serde::Deserialize<'de> for $name {
                fn deserialize<D: ::serde::Deserializer<'de>>(
                    deserializer: D,
                ) -> ::core::result::Result<Self, D::Error> {
                    <$crate::serde_object::Object<Mirror> as ::serde::Deserialize>::deserialize(
                        deserializer,
                    )
                    .map(|$crate::serde_object::Object(mirror)| Self {
                        $($field: mirror.$field),*
                    })
                }
            }
        };

        $crate::object_only!($($rest)*);
    };
    (
        @attrs [$($def:tt)*] [$($mirror:tt)*]
        $vis:vis enum $name:ident {
            $($variant:ident { $($(#[$field_meta:meta])* $field:ident : $ty:ty),* $(,)? }),+ $(,)?
        }
        $($rest:tt)*
    ) => {
        $($def)*
        $vis enum $name {
            $($variant { $($(#[$field_meta])* $field: $ty),* }),+
        }

        const _: () = {
            #[derive(::serde::Deserialize)]
            $($mirror)*
            enum Mirror {
                $($variant { $($(#[$field_meta])* $field: $ty),* }),+
            }

            impl<'de> ::serde::Deserialize<'de> for $name {
                fn deserialize<D: ::serde::Deserializer<'de>>(
                    deserializer: D,
                ) -> ::core::result::Result<Self, D::Error> {
                    <$crate::serde_object::Object<Mirror> as ::serde::Deserialize>::deserialize(
                        deserializer,
                    )
                    .map(|$crate::serde_object::Object(mirror)| match mirror {
                        $(Mirror::$variant { $($field),* } => Self::$variant { $($field),* }),+
                    })
                }
            }
        };

        $crate::object_only!($($rest)*);
    };
    () => {};
    ($($item:tt)+) => {
        $crate::object_only!(@attrs [] [] $($item)+);
    };
}
