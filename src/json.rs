use std::io::{self, Read};

use jaq_core::{
    load::{Arena, File, Loader},
    Compiler, Ctx, RcIter,
};
use jaq_json::Val;

use promkit_widgets::{
    jsonstream::jsonz,
    serde_json::{self, Deserializer, Value},
};
use serde::de::IgnoredAny;

/// Describes an input that was too large to load in full,
/// of which only the leading JSON values were loaded.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Preview {
    /// Number of JSON values loaded.
    pub values: usize,
    /// Number of bytes those values span.
    pub bytes: usize,
}

/// Read the input, loading at most `limit` bytes of it.
///
/// Inputs that fit within `limit` are read in full, exactly as before.
/// For larger inputs (e.g. multi-gigabyte JSON Lines dumps), only the
/// leading complete JSON values within `limit` bytes are kept and the rest
/// of the input is never read, so memory use does not grow with input size.
/// If not even the first value fits, the limit is doubled until it does,
/// so a single large JSON document still loads as a whole.
pub fn read_bounded(
    mut reader: impl Read,
    limit: usize,
    max_streams: Option<usize>,
) -> anyhow::Result<(String, Option<Preview>)> {
    let mut buf = Vec::new();
    let mut limit = limit.max(1);
    loop {
        // Read one byte past the limit to learn whether more input follows.
        let want = limit.saturating_add(1) - buf.len();
        (&mut reader).take(want as u64).read_to_end(&mut buf)?;
        if buf.len() <= limit {
            return Ok((into_string(buf)?, None));
        }

        let (values, bytes) = complete_prefix(&buf, max_streams)?;
        if values > 0 {
            buf.truncate(bytes);
            return Ok((into_string(buf)?, Some(Preview { values, bytes })));
        }
        limit = limit.saturating_mul(2);
    }
}

/// Count the complete JSON values at the start of `buf` (up to `max_streams`)
/// and the byte offset where the last of them ends.
/// `buf` is assumed to be cut off from a longer input.
fn complete_prefix(buf: &[u8], max_streams: Option<usize>) -> anyhow::Result<(usize, usize)> {
    let mut stream = Deserializer::from_slice(buf).into_iter::<IgnoredAny>();
    let (mut values, mut bytes) = (0, 0);
    while max_streams.is_none_or(|max| values < max) {
        match stream.next() {
            // A value running up to the cut may be truncated (e.g. `12` of `123`).
            Some(Ok(_)) if stream.byte_offset() == buf.len() => break,
            Some(Ok(_)) => {
                values += 1;
                bytes = stream.byte_offset();
            }
            // The value cut off by the limit.
            Some(Err(e)) if e.is_eof() => break,
            Some(Err(e)) => return Err(e.into()),
            None => break,
        }
    }
    Ok((values, bytes))
}

/// Same validation and error as `Read::read_to_string`.
fn into_string(buf: Vec<u8>) -> io::Result<String> {
    String::from_utf8(buf).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "stream did not contain valid UTF-8",
        )
    })
}

/// Get all JSON paths from the input JSON string,
/// respecting the max_streams limit if provided.
pub async fn get_all_paths(
    json_str: &str,
    max_streams: Option<usize>,
) -> anyhow::Result<impl Iterator<Item = String>> {
    let stream = deserialize(json_str, max_streams)?;
    let paths = jsonz::get_all_paths(stream.iter()).collect::<Vec<_>>();
    Ok(paths.into_iter())
}

/// Deserialize JSON string into a vector of serde_json::Value.
/// If max_streams is given, only deserialize up to that many JSON values.
pub fn deserialize(
    json_str: &str,
    max_streams: Option<usize>,
) -> anyhow::Result<Vec<serde_json::Value>> {
    let deserializer: serde_json::StreamDeserializer<'_, serde_json::de::StrRead<'_>, Value> =
        Deserializer::from_str(json_str).into_iter::<serde_json::Value>();
    let results = match max_streams {
        Some(l) => deserializer.take(l).collect::<Result<Vec<_>, _>>(),
        None => deserializer.collect::<Result<Vec<_>, _>>(),
    };
    results.map_err(anyhow::Error::from)
}

pub fn run_jaq(
    query: &str,
    json_stream: &[serde_json::Value],
) -> anyhow::Result<Vec<serde_json::Value>> {
    let arena = Arena::default();
    let loader = Loader::new(jaq_std::defs().chain(jaq_json::defs()));
    let modules = loader
        .load(
            &arena,
            File {
                code: query,
                path: (),
            },
        )
        .map_err(|errs| anyhow::anyhow!("jq filter parsing failed: {errs:?}"))?;
    let filter = Compiler::default()
        .with_funs(jaq_std::funs().chain(jaq_json::funs()))
        .compile(modules)
        .map_err(|errs| anyhow::anyhow!("jq filter compilation failed: {errs:?}"))?;

    let mut ret = Vec::<serde_json::Value>::new();

    for input in json_stream {
        let inputs = RcIter::new(core::iter::empty());
        let out = filter.run((Ctx::new([], &inputs), Val::from(input.clone())));
        for item in out {
            match item {
                Ok(val) => ret.push(val.into()),
                Err(err) => return Err(anyhow::anyhow!("jq filter execution failed: {err}")),
            }
        }
    }

    Ok(ret)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(input: &str, limit: usize, max_streams: Option<usize>) -> (String, Option<Preview>) {
        read_bounded(input.as_bytes(), limit, max_streams).unwrap()
    }

    #[test]
    fn input_within_limit_is_read_in_full() {
        let input = "{\"a\":1}\n{\"a\":2}\n";
        assert_eq!(read(input, input.len(), None), (input.to_string(), None));
        // Even invalid JSON is passed through untouched, as before.
        assert_eq!(read("{oops", 64, None), ("{oops".to_string(), None));
    }

    #[test]
    fn large_input_keeps_leading_complete_values() {
        let input = "{\"a\":1}\n{\"a\":2}\n{\"a\":3}\n";
        // The limit falls in the middle of the third value.
        let (text, preview) = read(input, 20, None);
        assert_eq!(text, "{\"a\":1}\n{\"a\":2}");
        assert_eq!(
            preview,
            Some(Preview {
                values: 2,
                bytes: 15
            })
        );
    }

    #[test]
    fn large_input_respects_max_streams() {
        let input = "1 2 3 4 5 6 7 8 9 10";
        let (text, preview) = read(input, 10, Some(2));
        assert_eq!(text, "1 2");
        assert_eq!(
            preview,
            Some(Preview {
                values: 2,
                bytes: 3
            })
        );
    }

    #[test]
    fn number_cut_by_limit_is_dropped() {
        // The limit cuts "333" to "3"; it must not show up as the value 3.
        let (text, preview) = read("1 22 333 4444", 6, None);
        assert_eq!(text, "1 22");
        assert_eq!(
            preview,
            Some(Preview {
                values: 2,
                bytes: 4
            })
        );
    }

    #[test]
    fn first_value_larger_than_limit_is_loaded_whole() {
        let doc = format!("[{}]", vec!["0"; 100].join(","));
        assert_eq!(read(&doc, 8, None), (doc.clone(), None));

        let input = format!("{doc}\n{doc}\n{doc}\n");
        let (text, preview) = read(&input, 8, None);
        assert_eq!(text, doc);
        assert_eq!(
            preview,
            Some(Preview {
                values: 1,
                bytes: doc.len()
            })
        );
    }

    #[test]
    fn syntax_error_in_large_input_is_reported() {
        assert!(read_bounded("{\"a\":1} {oops} {\"a\":2}".as_bytes(), 12, None).is_err());
    }

    #[test]
    fn invalid_utf8_is_rejected() {
        let err = read_bounded(&b"\"\xff\""[..], 64, None).unwrap_err();
        assert_eq!(err.to_string(), "stream did not contain valid UTF-8");
    }
}
