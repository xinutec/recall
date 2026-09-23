//! JSON written the way Python's `json.dumps` writes it, the spelling of the
//! JSON already stored in this database.
//!
//! `serde_json`'s defaults change the text but not the meaning: no space after
//! `,` and `:`, and non-ASCII written raw rather than escaped. Keeping one
//! spelling per column means a comparison of stored text finds only real
//! differences.

/// The formatter: `{"s": 0}`, not `serde_json`'s `{"s":0}`.
struct PythonJson;

impl serde_json::ser::Formatter for PythonJson {
    fn begin_array_value<W: ?Sized + std::io::Write>(
        &mut self,
        writer: &mut W,
        first: bool,
    ) -> std::io::Result<()> {
        if first {
            Ok(())
        } else {
            writer.write_all(b", ")
        }
    }

    fn begin_object_key<W: ?Sized + std::io::Write>(
        &mut self,
        writer: &mut W,
        first: bool,
    ) -> std::io::Result<()> {
        if first {
            Ok(())
        } else {
            writer.write_all(b", ")
        }
    }

    fn begin_object_value<W: ?Sized + std::io::Write>(
        &mut self,
        writer: &mut W,
    ) -> std::io::Result<()> {
        writer.write_all(b": ")
    }

    /// Non-ASCII is escaped as `\uXXXX` UTF-16 units, as `json.dumps` does by
    /// default (`ensure_ascii=True`).
    fn write_string_fragment<W: ?Sized + std::io::Write>(
        &mut self,
        writer: &mut W,
        fragment: &str,
    ) -> std::io::Result<()> {
        for ch in fragment.chars() {
            if ch.is_ascii() {
                write!(writer, "{ch}")?;
            } else {
                // Above the BMP: a surrogate pair, as Python writes it.
                let mut buf = [0u16; 2];
                for unit in ch.encode_utf16(&mut buf) {
                    write!(writer, "\\u{unit:04x}")?;
                }
            }
        }
        Ok(())
    }
}

/// Serialise as `json.dumps` does.
pub fn dump<T: serde::Serialize + ?Sized>(value: &T) -> String {
    let mut out = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut out, PythonJson);
    value.serialize(&mut ser).expect("serialise");
    String::from_utf8(out).expect("json is utf-8")
}
