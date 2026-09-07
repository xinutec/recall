//! JSON written the way `json.dumps` writes it.
//!
//! ⚠ Every JSON value already in this database was written by Python, and both
//! implementations write some of these columns. `serde_json`'s defaults differ in
//! two ways that change the TEXT without changing the meaning: it omits the space
//! after `,` and `:`, and it emits non-ASCII raw where `json.dumps` escapes it
//! (`ensure_ascii=True`). Two spellings of one value in one column is what makes
//! a later parity check report drift that is not drift.

/// The formatter. `serde_json` writes `{"s":0}`; `json.dumps` writes `{"s": 0}`. Every stored
/// `word_timings` blob was written by the latter.
///
/// ⚠ The column is read as JSON, so the spacing changes no meaning — but two
/// spellings of one value in one column is exactly what makes a later parity
/// check report drift that is not drift.
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

    /// ⚠ `json.dumps` defaults to `ensure_ascii=True`, so every stored blob
    /// spells `ë` as `\u00eb`. `serde_json` writes the character. A Dutch word in
    /// a meeting is enough to make the two disagree, and this archive is half
    /// Dutch.
    fn write_string_fragment<W: ?Sized + std::io::Write>(
        &mut self,
        writer: &mut W,
        fragment: &str,
    ) -> std::io::Result<()> {
        for ch in fragment.chars() {
            if ch.is_ascii() {
                write!(writer, "{ch}")?;
            } else {
                // Astral characters go out as a surrogate pair, which is what
                // Python writes for anything above the BMP.
                let mut buf = [0u16; 2];
                for unit in ch.encode_utf16(&mut buf) {
                    write!(writer, "\\u{unit:04x}")?;
                }
            }
        }
        Ok(())
    }
}

/// Serialise exactly as the Python writes it.
pub fn dump<T: serde::Serialize + ?Sized>(value: &T) -> String {
    let mut out = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut out, PythonJson);
    value.serialize(&mut ser).expect("serialise");
    String::from_utf8(out).expect("json is utf-8")
}
