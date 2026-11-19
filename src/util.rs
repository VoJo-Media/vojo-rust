use serde::{Deserialize, Deserializer, Serialize};

use crate::error::{Error, Result};

/// Replaces anything that looks like a secret key.
pub(crate) fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("vojo_") {
        let after = &rest[at + 5..];
        let mode = ["live_", "test_"].iter().find(|m| after.starts_with(*m));
        let tail = mode.map(|m| &after[m.len()..]).unwrap_or("");
        let len = tail.bytes().take_while(|b| b.is_ascii_alphanumeric() || *b == b'_').count();
        out.push_str(&rest[..at]);
        if mode.is_some() && len > 0 {
            out.push_str("<redacted>");
            rest = &tail[len..];
        } else {
            out.push_str("vojo_");
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

pub(crate) fn random_bytes<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0u8; N];
    getrandom::getrandom(&mut bytes).map_err(|e| {
        Error::InvalidArgument(format!("could not get random bytes from the operating system: {e}"))
    })?;
    Ok(bytes)
}

pub(crate) fn uuid_v4() -> Result<String> {
    let mut b = random_bytes::<16>()?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex = hex(&b);
    Ok(format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..]))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 0x0f) as usize] as char);
    }
    out
}

/// Percent-encodes everything but the unreserved characters.
pub(crate) fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Fills each `{name}` in a path. An empty id or a dot segment would silently
/// hit a different route, so those are refused.
pub(crate) fn expand_path(template: &str, params: &[(&str, &str)]) -> Result<String> {
    let mut path = template.to_owned();
    for (name, value) in params {
        if value.is_empty() || *value == "." || *value == ".." {
            return Err(Error::InvalidArgument(format!("{name} must be a real id, got {value:?}")));
        }
        path = path.replacen(&format!("{{{name}}}"), &encode(value), 1);
    }
    Ok(path)
}

pub(crate) fn to_json<T: Serialize + ?Sized>(body: &T) -> Result<Vec<u8>> {
    serde_json::to_vec(body).map_err(|e| Error::InvalidArgument(format!("could not encode the request: {e}")))
}

/// Reads a missing or null field as its default, as `/v1` may leave fields out.
pub(crate) fn null_default<'de, D, T>(deserializer: D) -> std::result::Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// A string enum from the API. Values this version does not know arrive as
/// `Unknown`, because `/v1` adds values without notice.
macro_rules! string_enum {
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident = $wire:literal,)* }) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Eq, Hash)]
        #[non_exhaustive]
        pub enum $name {
            $($(#[$vmeta])* $variant,)*
            /// A value this version of the library does not know.
            Unknown(String),
        }

        impl $name {
            /// The value as sent over the wire.
            pub fn as_str(&self) -> &str {
                match self {
                    $(Self::$variant => $wire,)*
                    Self::Unknown(value) => value,
                }
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::Unknown(String::new())
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                match value {
                    $($wire => Self::$variant,)*
                    other => Self::Unknown(other.to_owned()),
                }
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self::from(value.as_str())
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
                Ok(Self::from(String::deserialize(deserializer)?))
            }
        }
    };
}

pub(crate) use string_enum;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_keys_and_leaves_the_rest() {
        let live = format!("vojo_{}abc_123", "live_");
        let test = format!("vojo_{}XYZ", "test_");
        let text = format!("key {live}, other {test}. vojo_ vojo_live_ vojo_other_1");
        assert_eq!(redact(&text), "key <redacted>, other <redacted>. vojo_ vojo_live_ vojo_other_1");
    }

    #[test]
    fn uuid_is_version_4() {
        let id = uuid_v4().unwrap();
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "4");
        assert!(matches!(&id[19..20], "8" | "9" | "a" | "b"));
        assert_ne!(id, uuid_v4().unwrap());
    }

    #[test]
    fn paths_are_escaped_and_checked() {
        assert_eq!(expand_path("/v1/a/{x}/b", &[("x", "a b/c")]).unwrap(), "/v1/a/a%20b%2Fc/b");
        for bad in ["", ".", ".."] {
            assert!(matches!(expand_path("/v1/a/{x}", &[("x", bad)]), Err(Error::InvalidArgument(_))));
        }
    }
}
