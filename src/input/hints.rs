/// Generate hint labels (a, s, d, f, ...) for `n` clickable elements.
pub fn hint_labels(n: usize) -> Vec<String> {
    const ALPHA: &[u8] = b"asdfghjklqwertyuiopzxcvbnm";
    if n <= ALPHA.len() {
        (0..n).map(|i| (ALPHA[i] as char).to_string()).collect()
    } else {
        // Two-letter labels for larger counts.
        let mut out = Vec::with_capacity(n);
        'outer: for &a in ALPHA {
            for &b in ALPHA {
                out.push(format!("{}{}", a as char, b as char));
                if out.len() == n { break 'outer; }
            }
        }
        out
    }
}

use crate::browser::Clickable;

pub fn assign(clickables: &[Clickable]) -> Vec<(String, Clickable)> {
    let labels = hint_labels(clickables.len());
    labels.into_iter().zip(clickables.iter().copied()).collect()
}

#[cfg(test)]
mod assign_tests {
    use super::*;
    use crate::browser::Clickable;

    #[test]
    fn assigns_label_per_element() {
        let cs = vec![
            Clickable { x: 1.0, y: 2.0 },
            Clickable { x: 3.0, y: 4.0 },
        ];
        let pairs = assign(&cs);
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0].0, "a");
        assert_eq!(pairs[1].0, "s");
        assert_eq!(pairs[1].1.x, 3.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn single_letters_for_small_counts() {
        assert_eq!(hint_labels(3), vec!["a", "s", "d"]);
    }
    #[test]
    fn two_letters_when_exhausted() {
        let labels = hint_labels(30);
        assert_eq!(labels.len(), 30);
        assert_eq!(labels[26].len(), 2);
    }
}
