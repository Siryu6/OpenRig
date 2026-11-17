//! Text search of the plugin browser and the store: every word of the query
//! must appear in the entry's text, ignoring case and accents.

/// `text` in lower case and without accents, so "Écho" and "echo" match.
/// Covers the Latin letters plugin names use; other scripts stay as they are.
pub fn fold(text: &str) -> String {
    let mut folded = String::with_capacity(text.len());
    for c in text.chars().flat_map(char::to_lowercase) {
        match base_letters(c) {
            Some(base) => folded.push_str(base),
            // Combining accents of decomposed text
            None if ('\u{300}'..='\u{36f}').contains(&c) => {}
            None => folded.push(c),
        }
    }
    folded
}

/// The plain letters of an accented lower-case Latin letter
fn base_letters(c: char) -> Option<&'static str> {
    Some(match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => "a",
        'æ' => "ae",
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => "c",
        'ď' | 'đ' => "d",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => "e",
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => "g",
        'ĥ' | 'ħ' => "h",
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => "i",
        'ĵ' => "j",
        'ķ' => "k",
        'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => "l",
        'ñ' | 'ń' | 'ņ' | 'ň' => "n",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => "o",
        'œ' => "oe",
        'ŕ' | 'ŗ' | 'ř' => "r",
        'ś' | 'ŝ' | 'ş' | 'š' => "s",
        'ß' => "ss",
        'ţ' | 'ť' | 'ŧ' => "t",
        'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => "u",
        'ŵ' => "w",
        'ý' | 'ÿ' | 'ŷ' => "y",
        'ź' | 'ż' | 'ž' => "z",
        _ => return None,
    })
}

/// Whether every word of `query` appears in `folded`, a text already passed
/// through [`fold`]. An empty query matches everything.
pub fn matches(folded: &str, query: &str) -> bool {
    fold(query).split_whitespace().all(|w| folded.contains(w))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_case_and_accents() {
        assert_eq!(fold("Écho Délai"), "echo delai");
        assert_eq!(fold("Straße ŒUVRE"), "strasse oeuvre");
        // Decomposed accents too
        assert_eq!(fold("e\u{301}cho"), "echo");
        assert_eq!(fold("リバーブ"), "リバーブ");
    }

    #[test]
    fn matches_every_word() {
        let text = fold("Dragonfly Hall Reverb  ·  Michael Willis");
        assert!(matches(&text, ""));
        assert!(matches(&text, "  "));
        assert!(matches(&text, "hall"));
        assert!(matches(&text, "REVERB willis"));
        assert!(matches(&text, "réverb"));
        assert!(!matches(&text, "hall delay"));
        let text = fold("Peavey 5150 Blockletter @doubleyoud");
        assert!(matches(&text, "5150 peavey"));
        assert!(!matches(&text, "5150 mesa"));
    }
}
