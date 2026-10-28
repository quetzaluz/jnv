use std::io::{self, Read};

use promkit_widgets::serde_json::Deserializer;
use serde::de::IgnoredAny;

/// Most records a preview may hold. Memory use follows the number of records
/// rather than their size, so bytes alone don't bound it for tiny records.
const PREVIEW_MAX_RECORDS: usize = 100_000;

/// Describes an input that was cut down to a preview of its leading records.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Preview {
    /// Number of bytes kept from the head of the input.
    pub shown_bytes: usize,
    /// Size of the whole input, if known (e.g. not for pipes).
    pub total_bytes: Option<u64>,
}

impl Preview {
    /// Describe what the user is looking at.
    pub fn message(&self) -> String {
        let scope = match self.total_bytes {
            Some(total) => format!(
                "first {} of {}",
                format_size(self.shown_bytes as u64),
                format_size(total)
            ),
            None => format!("first {}", format_size(self.shown_bytes as u64)),
        };
        format!("Large input: previewing {scope}; filters and output cover only this part")
    }
}

/// Input data, together with how much of the source it represents.
#[derive(Debug)]
pub struct Input {
    pub text: String,
    pub preview: Option<Preview>,
}

fn format_size(bytes: u64) -> String {
    const KIB: f64 = (1 << 10) as f64;
    const MIB: f64 = (1 << 20) as f64;
    const GIB: f64 = (1 << 30) as f64;
    let bytes = bytes as f64;
    if bytes >= GIB {
        format!("{:.1} GiB", bytes / GIB)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes / MIB)
    } else {
        format!("{:.1} KiB", bytes / KIB)
    }
}

fn into_text(bytes: Vec<u8>) -> io::Result<String> {
    String::from_utf8(bytes).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "stream did not contain valid UTF-8",
        )
    })
}

/// Find where the preview should end: the end of the first complete JSON value
/// at or past `preview_size` bytes or [`PREVIEW_MAX_RECORDS`] records,
/// or the last complete value in `text`. If the user set `max_streams`, that
/// many records are wanted regardless of those limits.
/// Returns `None` if `text` does not hold a single complete value.
fn preview_end(text: &str, preview_size: usize, max_streams: Option<usize>) -> Option<usize> {
    let mut stream = Deserializer::from_str(text).into_iter::<IgnoredAny>();
    let mut end = 0;
    let mut records = 0;
    while let Some(value) = stream.next() {
        match value {
            Ok(_) => {
                end = stream.byte_offset();
                records += 1;
                let done = match max_streams {
                    Some(max_streams) => records >= max_streams,
                    None => end >= preview_size || records >= PREVIEW_MAX_RECORDS,
                };
                if done {
                    break;
                }
            }
            Err(e) if e.is_eof() => break,
            // Keep the malformed part so that it gets reported when the input is parsed.
            Err(_) => return Some(text.len()),
        }
    }
    (end > 0).then_some(end)
}

/// Read the input, keeping memory bounded for streams of JSON values.
///
/// Input of up to `threshold` bytes is returned as is. Anything larger is cut
/// down to its leading records, about `preview_size` bytes' worth, without
/// reading the rest. Input that has no complete record within `threshold` bytes
/// (e.g. one huge document) can't be previewed and is read entirely.
/// A `threshold` of 0 disables previews.
pub fn read(
    mut reader: impl Read,
    total_bytes: Option<u64>,
    threshold: usize,
    preview_size: usize,
    max_streams: Option<usize>,
) -> io::Result<Input> {
    let mut bytes = Vec::new();

    if threshold > 0 {
        reader
            .by_ref()
            .take(threshold as u64 + 1)
            .read_to_end(&mut bytes)?;

        if bytes.len() <= threshold {
            return Ok(Input {
                text: into_text(bytes)?,
                preview: None,
            });
        }

        // Cutting at a newline can't split a UTF-8 character or a top-level number.
        let complete_lines = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        let end = std::str::from_utf8(&bytes[..complete_lines])
            .map(|head| preview_end(head, preview_size, max_streams))
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "stream did not contain valid UTF-8",
                )
            })?;

        if let Some(end) = end {
            bytes.truncate(end);
            return Ok(Input {
                text: into_text(bytes)?,
                preview: Some(Preview {
                    shown_bytes: end,
                    total_bytes,
                }),
            });
        }
    }

    reader.read_to_end(&mut bytes)?;
    Ok(Input {
        text: into_text(bytes)?,
        preview: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json;

    const THRESHOLD: usize = 64;
    const PREVIEW_SIZE: usize = 16;

    fn read_str(input: &str) -> io::Result<Input> {
        read(input.as_bytes(), None, THRESHOLD, PREVIEW_SIZE, None)
    }

    fn ndjson(records: usize) -> String {
        (0..records).map(|i| format!("{{\"id\":{i}}}\n")).collect()
    }

    #[test]
    fn input_up_to_threshold_is_untouched() {
        let text = " ".repeat(THRESHOLD - 9) + "{\"a\": 1}\n";
        assert_eq!(text.len(), THRESHOLD);
        let input = read_str(&text).unwrap();
        assert_eq!(input.text, text);
        assert_eq!(input.preview, None);
    }

    #[test]
    fn stream_over_threshold_is_cut_at_a_record_boundary() {
        let text = ndjson(100);
        let input = read(
            text.as_bytes(),
            Some(text.len() as u64),
            THRESHOLD,
            PREVIEW_SIZE,
            None,
        )
        .unwrap();

        let values = json::deserialize(&input.text, None).unwrap();
        assert_eq!(values.len(), 2);
        assert_eq!(values[1]["id"], 1);
        assert_eq!(
            input.preview,
            Some(Preview {
                shown_bytes: input.text.len(),
                total_bytes: Some(text.len() as u64),
            })
        );
    }

    #[test]
    fn reader_is_not_drained_when_previewing() {
        let text = ndjson(100);
        let mut reader = text.as_bytes();
        read(&mut reader, None, THRESHOLD, PREVIEW_SIZE, None).unwrap();
        assert_eq!(reader.len(), text.len() - THRESHOLD - 1);
    }

    #[test]
    fn multibyte_character_at_the_read_limit_is_not_an_error() {
        // The read stops inside the last "é" of the record that crosses the limit.
        let text = format!("{}\n\"{}\"\n", "1\n".repeat(30), "é".repeat(20));
        assert!(std::str::from_utf8(&text.as_bytes()[..THRESHOLD + 1]).is_err());
        let input = read(text.as_bytes(), None, THRESHOLD, THRESHOLD * 2, None).unwrap();
        assert!(input.preview.is_some());
        assert_eq!(json::deserialize(&input.text, None).unwrap().len(), 30);
    }

    #[test]
    fn number_at_the_read_limit_is_not_truncated() {
        let text = format!("{}\n{}\n", " ".repeat(THRESHOLD - 3), "1".repeat(10));
        let input = read(text.as_bytes(), None, THRESHOLD, 1, None).unwrap();
        // The only line break precedes the number, so there is nothing to preview.
        assert_eq!(input.preview, None);
        assert_eq!(input.text, text);
    }

    #[test]
    fn single_document_over_threshold_is_read_entirely() {
        for text in [
            // One line.
            format!("[{}1]", "1,".repeat(THRESHOLD)),
            // Pretty printed.
            format!("[\n{}1\n]\n", "1,\n".repeat(THRESHOLD)),
        ] {
            let input = read_str(&text).unwrap();
            assert_eq!(input.text, text);
            assert_eq!(input.preview, None);
        }
    }

    #[test]
    fn pretty_printed_stream_is_cut_after_a_complete_value() {
        let text = "{\n  \"id\": 1\n}\n".repeat(20);
        let input = read_str(&text).unwrap();
        assert!(input.preview.is_some());
        assert_eq!(json::deserialize(&input.text, None).unwrap().len(), 2);
    }

    #[test]
    fn tiny_records_are_capped_by_count() {
        let text = "1\n".repeat(PREVIEW_MAX_RECORDS + 10);
        let end = preview_end(&text, usize::MAX, None).unwrap();
        let values = json::deserialize(&text[..end], None).unwrap();
        assert_eq!(values.len(), PREVIEW_MAX_RECORDS);
    }

    #[test]
    fn max_streams_overrides_preview_limits() {
        let text = ndjson(100);
        let shown = |max_streams| {
            let input = read(text.as_bytes(), None, THRESHOLD, PREVIEW_SIZE, max_streams).unwrap();
            json::deserialize(&input.text, None).unwrap().len()
        };
        assert_eq!(shown(Some(1)), 1);
        // Beyond the 2 records `PREVIEW_SIZE` allows, up to what the read limit holds.
        assert_eq!(shown(Some(6)), 6);
    }

    #[test]
    fn record_larger_than_preview_size_is_kept_whole() {
        let record = format!("{{\"a\":\"{}\"}}\n", "x".repeat(PREVIEW_SIZE * 2));
        let input = read_str(&record.repeat(10)).unwrap();
        assert!(input.preview.is_some());
        assert_eq!(input.text.trim_end(), record.trim_end());
    }

    #[test]
    fn malformed_record_within_preview_still_fails_to_parse() {
        let text = format!("{{\"id\":0}}\n{{oops\n{}", ndjson(100));
        let input = read_str(&text).unwrap();
        let err = json::deserialize(&input.text, None).unwrap_err();
        assert!(err.to_string().contains("line 2"), "{err}");
    }

    #[test]
    fn invalid_utf8_is_an_error() {
        for bytes in [vec![b'"', 0xff, b'"', b'\n'], {
            let mut bytes = ndjson(100).into_bytes();
            bytes[3] = 0xff;
            bytes
        }] {
            let err = read(bytes.as_slice(), None, THRESHOLD, PREVIEW_SIZE, None).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        }
    }

    #[test]
    fn zero_threshold_disables_previews() {
        let text = ndjson(100);
        let input = read(text.as_bytes(), None, 0, PREVIEW_SIZE, None).unwrap();
        assert_eq!(input.text, text);
        assert_eq!(input.preview, None);
    }

    #[test]
    fn preview_message_mentions_total_only_when_known() {
        let mut preview = Preview {
            shown_bytes: 16 << 20,
            total_bytes: Some(5 << 30),
        };
        assert!(preview.message().contains("first 16.0 MiB of 5.0 GiB"));
        preview.total_bytes = None;
        assert!(preview.message().contains("first 16.0 MiB;"));
        preview.shown_bytes = 3 << 10;
        assert!(preview.message().contains("first 3.0 KiB;"));
    }
}
