use crate::text2voice::Text2VoiceRequest;

const CONTRACTIONS: &[(&str, &str)] = &[
    ("can't", "cannot"),
    ("can t", "cannot"),
    ("won't", "will not"),
    ("won t", "will not"),
    ("don't", "do not"),
    ("don t", "do not"),
    ("doesn't", "does not"),
    ("didn't", "did not"),
    ("isn't", "is not"),
    ("aren't", "are not"),
    ("wasn't", "was not"),
    ("weren't", "were not"),
    ("haven't", "have not"),
    ("hasn't", "has not"),
    ("hadn't", "had not"),
    ("wouldn't", "would not"),
    ("shouldn't", "should not"),
    ("couldn't", "could not"),
    ("mustn't", "must not"),
    ("needn't", "need not"),
    ("i'm", "I am"),
    ("you're", "you are"),
    ("we're", "we are"),
    ("they're", "they are"),
    ("it's", "it is"),
    ("that's", "that is"),
    ("there's", "there is"),
    ("here's", "here is"),
    ("i've", "I have"),
    ("we've", "we have"),
    ("they've", "they have"),
    ("i'll", "I will"),
    ("you'll", "you will"),
    ("we'll", "we will"),
    ("they'll", "they will"),
    ("i'd", "I would"),
    ("you'd", "you would"),
    ("we'd", "we would"),
    ("they'd", "they would"),
];

pub(crate) fn prepare_text2voice_request(request: &Text2VoiceRequest) -> Text2VoiceRequest {
    Text2VoiceRequest {
        text: prepare_speech_text(&request.text),
        voice_id: request.voice_id.clone(),
        model: request.model.clone(),
    }
}

pub(crate) fn prepare_speech_text(text: &str) -> String {
    let mut prepared = canonicalize_apostrophes(text);
    for (pattern, replacement) in CONTRACTIONS {
        prepared = replace_phrase(&prepared, pattern, replacement);
    }
    prepared
}

fn canonicalize_apostrophes(text: &str) -> String {
    text.replace(['\u{2018}', '\u{2019}', '\u{0060}', '\u{00b4}'], "'")
}

fn replace_phrase(input: &str, phrase: &str, replacement: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let lower = input.to_ascii_lowercase();
    let mut cursor = 0;
    while let Some(relative_start) = lower[cursor..].find(phrase) {
        let start = cursor + relative_start;
        let end = start + phrase.len();
        if !has_phrase_boundary(input, start, end) {
            output.push_str(&input[cursor..end]);
            cursor = end;
            continue;
        }
        output.push_str(&input[cursor..start]);
        output.push_str(&case_aware_replacement(input, start, replacement));
        cursor = end;
    }
    output.push_str(&input[cursor..]);
    output
}

fn has_phrase_boundary(input: &str, start: usize, end: usize) -> bool {
    !previous_is_word(input, start) && !next_is_word(input, end)
}

fn previous_is_word(input: &str, start: usize) -> bool {
    input[..start]
        .chars()
        .next_back()
        .is_some_and(|character| character.is_ascii_alphanumeric())
}

fn next_is_word(input: &str, end: usize) -> bool {
    input[end..]
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_alphanumeric())
}

fn case_aware_replacement(input: &str, start: usize, replacement: &str) -> String {
    let Some(first) = input[start..].chars().next() else {
        return replacement.to_string();
    };
    if !first.is_ascii_uppercase() {
        return replacement.to_string();
    }
    capitalize_first_ascii(replacement)
}

fn capitalize_first_ascii(value: &str) -> String {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    format!("{}{}", first.to_ascii_uppercase(), chars.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepare_speech_text_expands_common_contractions() {
        assert_eq!(
            prepare_speech_text("I can't do that, but I'm checking."),
            "I cannot do that, but I am checking."
        );
    }

    #[test]
    fn prepare_speech_text_handles_curly_apostrophes() {
        assert_eq!(
            prepare_speech_text("You’re right; it’s not ready."),
            "You are right; it is not ready."
        );
    }

    #[test]
    fn prepare_speech_text_repairs_split_cannot() {
        assert_eq!(prepare_speech_text("I can t hear it."), "I cannot hear it.");
    }

    #[test]
    fn prepare_speech_text_does_not_rewrite_inside_words() {
        assert_eq!(prepare_speech_text("scant data"), "scant data");
    }
}
