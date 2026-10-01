//! Word errors, spelled as `src/recall/wer.py` spells them: lowercase, every
//! non-word character a space, then a word-level edit distance.

/// The words of `text` after normalisation.
pub fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

/// An alignment's errors against `reference` words.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct Errors {
    pub reference: usize,
    pub substitutions: usize,
    pub deletions: usize,
    pub insertions: usize,
}

impl Errors {
    /// Errors over reference words; `None` with no reference, where a rate is undefined.
    pub fn rate(&self) -> Option<f64> {
        (self.reference > 0).then(|| {
            (self.substitutions + self.deletions + self.insertions) as f64 / self.reference as f64
        })
    }

    pub fn add(&mut self, other: Self) {
        self.reference += other.reference;
        self.substitutions += other.substitutions;
        self.deletions += other.deletions;
        self.insertions += other.insertions;
    }
}

/// A minimum-edit alignment of `hypothesis` against `reference`, counted by kind.
pub fn align(reference: &[String], hypothesis: &[String]) -> Errors {
    // Each cell: (total, substitutions, deletions, insertions); ties prefer the
    // fewest substitutions, then deletions, so counts are deterministic.
    type Cell = (usize, usize, usize, usize);
    let mut previous: Vec<Cell> = (0..=hypothesis.len()).map(|j| (j, 0, 0, j)).collect();
    for (i, r) in reference.iter().enumerate() {
        let mut current: Vec<Cell> = vec![(i + 1, 0, i + 1, 0)];
        for (j, h) in hypothesis.iter().enumerate() {
            let diagonal = previous[j];
            let substitute = if r == h {
                diagonal
            } else {
                (diagonal.0 + 1, diagonal.1 + 1, diagonal.2, diagonal.3)
            };
            let up = previous[j + 1];
            let delete = (up.0 + 1, up.1, up.2 + 1, up.3);
            let left = current[j];
            let insert = (left.0 + 1, left.1, left.2, left.3 + 1);
            current.push(
                [substitute, delete, insert]
                    .into_iter()
                    .min()
                    .unwrap_or(substitute),
            );
        }
        previous = current;
    }
    let (_, substitutions, deletions, insertions) = previous[hypothesis.len()];
    Errors {
        reference: reference.len(),
        substitutions,
        deletions,
        insertions,
    }
}
