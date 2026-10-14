// Copyright 2021-Present Datadog, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::sync::LazyLock;

use jieba_rs::Jieba;
use tantivy::tokenizer::{Token, TokenStream, Tokenizer};

/// Dictionary-based Chinese tokenizer.
///
/// `chinese_compatible` splits every Han character into its own token, so
/// `你好世界` becomes four tokens and a query for `天气` matches any document
/// holding a `天` and a `气`, adjacent or not. This tokenizer consults the
/// `jieba` dictionary instead, so multi-character words stay single tokens.
///
/// It runs jieba in search engine mode, which additionally emits the sub-words
/// of a longer word: `中国科学院` is indexed as `中国`, `科学`, `学院`,
/// `科学院` and `中国科学院`. Query strings run through this same tokenizer, so
/// a query for `中国科学院` expands to that same sequence of tokens and keeps
/// matching, even though jieba would segment the two strings differently in
/// precise mode.
///
/// # Hidden contracts
///
/// * `jieba-rs` must be built with its `default-dict` feature. It is part of jieba-rs' default
///   features, and disabling it removes `Jieba::new` altogether, so violating this contract breaks
///   the build rather than silently indexing with an empty dictionary.
/// * The dictionary is loaded once per process, on first use, and takes a few hundred milliseconds.
///   Never build a `Jieba` per token stream.
#[derive(Clone, Default)]
pub(crate) struct ChineseDictTokenizer;

/// The bundled dictionary is several megabytes and takes a few hundred
/// milliseconds to load, so it is built once per process rather than per token
/// stream.
static JIEBA: LazyLock<Jieba> = LazyLock::new(Jieba::new);

/// Returns whether a token may reach the inverted index.
///
/// jieba emits whitespace, punctuation and emoji as tokens of their own, whereas
/// `chinese_compatible` drops every non-alphanumeric character. Dropping the
/// tokens that carry neither a letter nor a digit keeps the two tokenizers
/// consistent on that point.
fn is_searchable(word: &str) -> bool {
    word.chars().any(char::is_alphanumeric)
}

pub(crate) struct ChineseDictTokenStream<'a> {
    text: &'a str,
    /// Byte ranges of the searchable tokens, in jieba's output order.
    ///
    /// Search engine mode emits the sub-words of a long word before the word
    /// itself, so these ranges overlap and are not monotonically increasing.
    tokens: Vec<(usize, usize)>,
    position: usize,
    token: Token,
}

impl TokenStream for ChineseDictTokenStream<'_> {
    fn advance(&mut self) -> bool {
        let Some(&(offset_from, offset_to)) = self.tokens.get(self.position) else {
            return false;
        };
        let position = self.position;
        self.position += 1;
        // Copy the reference out before taking the mutable borrow of `self`.
        let text = self.text;
        let token = self.token_mut();
        token.text.clear();
        token.text.push_str(&text[offset_from..offset_to]);
        token.offset_from = offset_from;
        token.offset_to = offset_to;
        token.position = position;
        token.position_length = 1;
        true
    }

    fn token(&self) -> &Token {
        &self.token
    }

    fn token_mut(&mut self) -> &mut Token {
        &mut self.token
    }
}

impl Tokenizer for ChineseDictTokenizer {
    type TokenStream<'a> = ChineseDictTokenStream<'a>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        // `hmm = true` lets jieba recover words that its dictionary does not
        // hold, such as product names and error codes.
        //
        // Hidden contract: positions are assigned by counting the emitted
        // tokens, so they are strictly increasing and dense. Tantivy stores
        // positions as deltas, so they must never decrease. Byte offsets cannot
        // be used as positions: search engine mode emits overlapping ranges.
        //
        // Offsets come from `byte_start`/`byte_end`. The `start`/`end` fields of
        // `jieba_rs::Token` are character indices and cannot slice `text`.
        let tokens = JIEBA
            .cut_for_search(text, true)
            .into_iter()
            .filter(|token| is_searchable(token.word))
            .map(|token| (token.byte_start, token.byte_end))
            .collect();
        ChineseDictTokenStream {
            text,
            tokens,
            position: 0,
            token: Token::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use tantivy::tokenizer::{TokenStream, Tokenizer};

    use super::ChineseDictTokenizer;

    fn tokens_with_positions(text: &str) -> Vec<(String, usize)> {
        let mut tokenizer = ChineseDictTokenizer;
        let mut stream = tokenizer.token_stream(text);
        let mut tokens = Vec::new();
        while stream.advance() {
            let token = stream.token();
            tokens.push((token.text.clone(), token.position));
        }
        tokens
    }

    fn tokens_of(text: &str) -> Vec<String> {
        tokens_with_positions(text)
            .into_iter()
            .map(|(text, _position)| text)
            .collect()
    }

    fn owned(strs: &[&str]) -> Vec<String> {
        strs.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn test_chinese_dict_tokenizer_keeps_words() {
        let tokens = tokens_of("今天天气很好, hello world");
        // The whole point of this tokenizer: `今天天气` is a dictionary word and
        // stays a single token, and the queryable words inside it are emitted
        // too.
        assert_eq!(
            tokens,
            owned(&[
                "今天",
                "天天",
                "天气",
                "今天天气",
                "很",
                "好",
                "hello",
                "world"
            ])
        );
    }

    #[test]
    fn test_chinese_dict_tokenizer_drops_punctuation() {
        let tokens = tokens_of("你好，世界！code=500 🚀");
        assert_eq!(tokens, owned(&["你好", "世界", "code", "500"]));
        // jieba emits whitespace, punctuation and emoji as tokens of their own;
        // none of them may reach the inverted index.
        assert_eq!(tokens_of("，。！, "), Vec::<String>::new());
        assert_eq!(tokens_of("🚀 🚀"), Vec::<String>::new());
        assert_eq!(tokens_of(""), Vec::<String>::new());
    }

    #[test]
    fn test_search_mode_emits_the_sub_words_of_a_long_word() {
        // Ground truth of jieba-rs 0.11.0 with its bundled dictionary. It pins
        // the token sequence the index depends on.
        assert_eq!(
            tokens_of("毕业于中国科学院计算所"),
            owned(&[
                "毕业",
                "于",
                "中国",
                "科学",
                "学院",
                "科学院",
                "中国科学院",
                "计算",
                "计算所",
            ])
        );
    }

    #[test]
    fn test_query_sub_words_line_up_with_indexed_positions() {
        // What search engine mode buys: the tokens of the query are a
        // contiguous slice of the tokens of the indexed text, so a phrase query
        // still matches even though jieba segments query and document
        // differently in precise mode (`中国科学院` is one token there).
        let indexed = tokens_of("毕业于中国科学院计算所");
        let queried = tokens_of("中国科学院");
        assert!(
            indexed
                .windows(queried.len())
                .any(|window| window == queried.as_slice()),
            "indexed={indexed:?} queried={queried:?}"
        );
    }

    #[test]
    fn test_offsets_are_byte_offsets() {
        // `jieba_rs::Token` exposes both character indices (`start`/`end`) and
        // byte offsets (`byte_start`/`byte_end`), and mixing them up only shows
        // on multi-byte input, so the text here is deliberately multi-byte.
        let text = "错误 emoji 🚀 code=500";
        {
            let mut tokenizer = ChineseDictTokenizer;
            let mut stream = tokenizer.token_stream(text);
            assert!(stream.advance());
            assert_eq!(stream.token().text, "错误");
            // Two characters, six bytes.
            assert_eq!(stream.token().offset_to, 6);
        }

        let mut tokenizer = ChineseDictTokenizer;
        let mut stream = tokenizer.token_stream(text);
        while stream.advance() {
            let token = stream.token();
            assert!(text.is_char_boundary(token.offset_from));
            assert!(text.is_char_boundary(token.offset_to));
            assert_eq!(&text[token.offset_from..token.offset_to], token.text);
        }
    }

    #[test]
    fn test_positions_are_dense() {
        // Dropped tokens consume no position, and neither the query side nor the
        // index side can see them, so phrase queries stay aligned.
        let positions: Vec<usize> = tokens_with_positions("你好，世界！code=500")
            .into_iter()
            .map(|(_text, position)| position)
            .collect();
        assert_eq!(positions, vec![0, 1, 2, 3]);
    }

    #[test]
    fn test_differs_from_chinese_compatible() {
        // The behaviour this tokenizer exists to change.
        assert_eq!(tokens_of("你好世界"), owned(&["你好", "世界"]));

        let tokenizer_manager = crate::create_default_quickwit_tokenizer_manager();
        let mut chinese_compatible = tokenizer_manager
            .get_tokenizer("chinese_compatible")
            .unwrap();
        let mut stream = chinese_compatible.token_stream("你好世界");
        let mut single_chars = Vec::new();
        while stream.advance() {
            single_chars.push(stream.token().text.clone());
        }
        assert_eq!(single_chars, owned(&["你", "好", "世", "界"]));
    }
}
