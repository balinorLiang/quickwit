# 中文词典分词实施方案(内置 Tokenizer)

**状态**:已实现,`quickwit-query` 单元测试全绿(见第 13 节实现状态);三项关键决策见第 1 节
**目标仓库**:Quickwit(`v0.9.1`,HEAD `a73912aec6f41c33e82e5e1cd18fcb9c7e56aeea`,2026-10-13)
**方案**:新增内置词典分词器 `chinese_dict`(基于 `jieba-rs`),无需修改索引配置 Schema
**修订日期**:2026-10-14(初版事实核对与设计决策修订)
**文档位置**:仓库根目录(非 `docs/`,避免影响 Docusaurus 构建)

---

## 0. 结论摘要

Quickwit 目前对中文的支持是**逐字切分**(内置分词器 `chinese_compatible`,把每个汉字拆成一个 token),**不是词典分词**。本方案通过给 `quickwit-query` 增加一个内置分词器来引入真正的词典分词能力:

- 新增 1 个文件、修改 4 个文件、新增 1 个依赖
- **不修改**索引配置的 Schema(`TokenizerType` 枚举无需改动)
- 注册后用户即可在索引配置中直接写 `tokenizer: chinese_dict`
- 索引侧与查询侧自动共享同一个 `TokenizerManager`,分词结果天然一致

**方案可行,但有 3 个必须由评审明确接受的代价**:词典的 **32 MiB 常驻内存**(只由"实际用该分词器分过词"的进程支付,见 13.6)、滚动升级时序约束、"必须重建索引且无回滚路径"。

---

## 1. 已定决策(2026-10-14)

| # | 决策项 | 结论 | 理由 |
|---|---|---|---|
| D1 | `jieba-rs` 版本 | **`0.11.0`**(当前最新) | 词典数据由默认 feature `default-dict` 提供,必须保留默认 feature;禁用后 `Jieba::new` 不存在,**编译期即失败**,不会静默退化为空词典 |
| D2 | 分词模式 | **`cut_for_search`(搜索引擎模式)** | 长词会额外输出所含子词,使"索引期切分 ≠ 查询期切分"造成的漏召回大幅减少;日志检索召回优先于精度 |
| D3 | 标点/空白 token | **过滤丢弃**(token 中不含任何字母或数字字符即丢弃) | 与 `chinese_compatible` 的 `Grouping::SplitIgnore` 行为对齐,避免 `，`、`,`、`!` 进入倒排 |

**D2 是初版遗漏的核心取舍**,原方案用 `Jieba::cut`(精确模式)。举例说明差异:

| 输入 | `cut`(精确模式) | `cut_for_search`(搜索引擎模式) |
|---|---|---|
| `小明硕士毕业于中国科学院计算所` | `小明/硕士/毕业/于/中国科学院/计算所` | `小明/硕士/毕业/于/中国/科学/学院/科学院/中国科学院/计算/计算所` |

精确模式下,查询串 `中国科学院计算所` 在索引期被整体切成 2 个 token,若查询期因为上下文不同被切得更细(或反之),短语查询即失配。搜索引擎模式让长词既保留整体、又暴露局部,显著降低这种失配概率。

> **隐藏契约(D2)**:`cut_for_search` 对同一长词输出的 token 顺序是"先子词、后整词"(如 `科学` → `学院` → `科学院` → `中国科学院`),因此**偏移区间重叠且非单调**。位置赋值必须用枚举序号(见 4.3 隐藏契约),不能改用字节偏移。

---

## 2. 背景:为什么纯配置做不到

### 2.1 两个硬约束(已在代码中核实)

**约束一:配置可声明的分词器类型只有 4 种**

`quickwit/quickwit-doc-mapper/src/doc_mapper/tokenizer_entry.rs:118`

```rust
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TokenizerType {
    Ngram(NgramTokenizerOption),    // 字符滑窗
    Regex(RegexTokenizerOption),    // 正则
    Simple,                         // 按非字母数字切分
    SourceCode,                     // 源代码
}
```

过滤器同样只有 3 种(`tokenizer_entry.rs:90` 的 `TokenFilterType`):`RemoveLong`、`LowerCaser`、`AsciiFolding`。**没有任何词典类实现。**

**约束二:未注册的分词器名会被直接拒绝**

`quickwit/quickwit-doc-mapper/src/doc_mapper/doc_mapper_impl.rs:357-379`(`validate_fields_tokenizers`),错误消息在 `:375`

```rust
if let Some(tokenizer_name) = tokenizer_name_opt
    && tokenizer_manager.get_tokenizer(tokenizer_name).is_none()
{
    bail!(
        "unknown tokenizer `{}` for field `{}`",
        tokenizer_name,
        field_entry.name()
    );
}
```

即:配置里写一个不存在的分词器名,建索引时会失败。**分词器必须先被注册。**

### 2.2 当前实际注册的分词器(共 10 个)

`quickwit/quickwit-query/src/tokenizers/mod.rs:36-94`,`create_default_quickwit_tokenizer_manager()`:

| 名称 | 构成 |
|---|---|
| `raw` | `RawTokenizer` + `RemoveLong` |
| `raw_lowercase` | `RawTokenizer` + `LowerCaser` + `RemoveLong` |
| `lowercase` | 同上(与 `raw_lowercase` 实现相同) |
| `default` | `SimpleTokenizer` + `RemoveLong` + `LowerCaser` |
| `whitespace` | `WhitespaceTokenizer` |
| `chinese_compatible` | `ChineseTokenizer`(逐字切分)+ `RemoveLong` + `LowerCaser`(`mod.rs:66`) |
| `source_code_default` | `CodeTokenizer` + `RemoveLong` + `LowerCaser` + `AsciiFolding` |
| `source_code_with_hex` | `CodeTokenizer::with_hex_support()` + 同上 |
| `unicode_segmenter` | `UnicodeSegmenterTokenizer` + `LowerCaser` + `RemoveLong` |
| `datadog` | 与 `unicode_segmenter` 同一个实例 |

### 2.3 现有中文分词的实际行为

`quickwit/quickwit-query/src/tokenizers/chinese_compatible.rs` 的实现是状态机:汉字逐个成 token(`Grouping::SplitKeep`),拉丁字母按空白/标点成组(`Grouping::Keep`),其他符号丢弃(`Grouping::SplitIgnore`)。CJK 覆盖区段为 `\u{4500}..=\u{9FFF}` 及扩展 A/B/C/D/E(`chinese_compatible.rs:51` 起,扩展 C/D/E 合并为 `2A700–2CEAF`)。

其单测给出的确切输出(`chinese_compatible.rs:131`):

```text
输入: "Hello world, 你好世界, bonjour monde"
输出: hello / world / 你 / 好 / 世 / 界 / bonjour / monde
```

**问题**:`你好世界` 变成 4 个单字 token。这意味着

- 召回率高(每个字都能命中)
- 但**没有"词"的概念**:搜 `天气` 会命中含 `天` 和 `气` 但不相邻的文档
- 索引体积更大(每个汉字一个倒排词条)

**本方案的目标**:引入词典,让 `天气`、`今天`、`服务器` 这类词成为**单个 token**,同时靠 D2 保留子词召回能力。

---

## 3. 候选路线对比(为什么选方案 B)

| 路线 | 改动范围 | 真词典分词 | 工作量 | 主要代价 |
|---|---|---|---|---|
| **A. 摄入前预分词** | 仅数据流 + 索引配置 | ✅(外部 jieba) | 小 | **查询侧必须同样预分词**;用户输入 `天气很好` 会被 `default` 当成单个 token,匹配失败;所有客户端/采集器都要改 |
| **B. 新增内置分词器**(本方案) | `quickwit-query` + 依赖 | ✅ | 中 | 新增依赖;需过许可/`machete` 闸门;所有节点都带词典 |
| **C. 恢复 tantivy `multilang`(lindera)** | tantivy features + 注册 | ✅ | 中 | 依赖树巨大;上游 0.9.0 刻意移除(#6154) |
| **D. 暴露为可配置类型** | doc-mapper + query | ✅ | 大 | 词典文件需在每个节点分发/加载;Schema 变更 |

### 选择 B 的关键理由

**索引与查询一致性**。方案 A 的致命问题是索引侧和查询侧必须各自分词,而用户通过 Quickwit 查询时,查询串是交给**字段的 tokenizer** 处理的——你无法在 Quickwit 内部对查询串做词典分词。方案 B 中索引与查询共用同一个 `TokenizerManager`,一致性由架构保证。

### 为什么不用方案 C(lindera)

**有历史证据**:生成物 `quickwit/dependency-licenses.html:14539` 起存在

```text
lindera-cc-cedict 0.27.2      ← 中文 CC-CEDICT 词典
lindera-ipadic 0.27.2         ← 日文
lindera-ko-dic 0.27.2         ← 韩文
```

说明 Quickwit 曾通过 tantivy 的 `multilang` feature 提供过词典级中日韩分词。但现状是:

- `multilang` 在 `quickwit/Cargo.toml` 与所有 `.rs` 文件中**零命中**
- `quickwit/Cargo.toml` 中 tantivy 的 features 仅:`lz4-compression`、`mmap`、`quickwit`、`zstd-compression`、`columnar-zstd-compression`
- CHANGELOG `[0.9.0]`(`CHANGELOG.md:38-39`)明确记录两条迁移:
  - *"Stemming is now restricted to the `multilang` cargo feature (#6085)"*
  - *"The unused multilang tokenizer feature was removed (#6154)"*

即上游**主动删除**了这条通道,重新引入需要对抗上游决策,且依赖树包含 ipadic / ko-dic / cedict 及各自的 builder,体积庞大。

---

## 4. 详细设计

### 4.1 依赖选择

| 候选 | 许可 | 词典 | 说明 |
|---|---|---|---|
| **`jieba-rs` 0.11.0(选用)** | **MIT**(已核实 crate manifest) | 内置,由默认 feature `default-dict` 提供 | 纯 Rust;API 简单;词典随 crate 分发无需外部文件 |
| `cang-jie` | MIT | 内置 | 支持简繁;但依赖 `lindera` 系,较重 |
| `lindera` | MIT(crate)/数据许可需单独核对 | 需选 `lindera-cc-cedict` | CC-CEDICT 数据常用 CC BY-SA,与 Apache-2.0 分发需评估 |

选用 `jieba-rs` 的理由:纯 Rust、无需外部词典文件分发。

**许可(已确认,非阻塞)**:解包 `jieba-rs 0.11.0` 的 `Cargo.toml` 可见 `license = "MIT"`,`quickwit/deny.toml:65` 的允许列表含 `"MIT"`,许可闸门可通过。此前"sparse index 中 `license` 字段为 null 故需确认"的判断不成立——以被解包 crate 的 manifest 为准。

**0.11.0 的依赖增量**(取自 crates.io sparse index):`cedarwood ^0.5`、`jieba-macros ^0.11`、`phf ^0.13.1`、`bytecount ^0.6`、`include-flate ^0.3.1`(可选)、`ordered-float ^5.0`(可选);`regex ^1.11.2` 与 `rustc-hash ^2.1` 已在 workspace / `quickwit-query` 依赖中。规模可控。

> **关于 feature(修订,以 crate 源码为准)**:`0.11.0` 的 `Cargo.toml` 中 `default = ["default-dict"]`、`default-dict = ["dep:include-flate"]`,即**词典仍由默认 feature 提供,初版那条"不得关闭默认 feature"的告警是对的**。我在上一修订版中据 crates.io sparse index 判定"0.9.0 起词典无条件内置",那是误读:带 `dep:` 语法的 feature 存放在 index 的 `features2` 字段,`features` 因此看起来是空的。落地时保持默认 feature 开启即可;若有人加上 `default-features = false`,`Jieba::new` 会消失、编译失败,属显式失败而非静默降级。
>
> **API 已实测确认**(直接阅读 `jieba-rs 0.11.0` 源码):`pub fn new() -> Self` 免参;`pub fn cut_for_search<'a>(&self, sentence: &'a str, hmm: bool) -> Vec<Token<'a>>`;`Token { word, start, end, byte_start, byte_end }` 中 **`start`/`end` 是字符下标,`byte_start`/`byte_end` 才是字节偏移**;`Jieba` 只含 `Vec`/`CharTrie`/`Option<HmmModel>` 等无内部可变性的字段,是 `Send + Sync`,`LazyLock` 静态实例成立。

### 4.2 变更文件清单

| # | 文件 | 操作 |
|---|---|---|
| 1 | `quickwit/quickwit-query/src/tokenizers/chinese_dict.rs` | **新增** |
| 2 | `quickwit/quickwit-query/src/tokenizers/mod.rs` | 修改(模块声明 + 注册) |
| 3 | `quickwit/quickwit-query/Cargo.toml` | 修改(加依赖) |
| 4 | `quickwit/Cargo.toml` | 修改(`[workspace.dependencies]` 加依赖) |
| 5 | `docs/configuration/index-config.md` | 修改(**新增对外文档**,见 4.6) |

**不需要**修改:`quickwit-doc-mapper`(`TokenizerType` 不变)、proto、OpenAPI Schema、REST 层、`query_ast/wildcard_query.rs`(见 4.5)。

### 4.3 新增文件:`quickwit/quickwit-query/src/tokenizers/chinese_dict.rs`

> **以下代码为设计稿,已落地到上述路径(以实际文件为准)**,落地时有 4 处修正是编译/实测才暴露的,稿件已就地改正:
>
> 1. **必须用 `byte_start`/`byte_end`**:`jieba_rs::Token` 的 `start`/`end` 是**字符下标**,用它去切 `&text[..]` 会在非 ASCII 输入上切错甚至 panic。这是本方案最容易踩的坑——只在多字节输入上暴露,单测必须用中文/emoji 才能覆盖。
> 2. **`advance` 需先把 `self.text` 拷进局部变量**再调 `self.token_mut()`,否则 `E0502`(不能同时可变借用 `self` 与不可变借用 `self.text`)。
> 3. 类型为 `pub(crate)`,与 `chinese_compatible` 一致,不是 `pub`。
> 4. 测试模块需 `use tantivy::tokenizer::{TokenStream, Tokenizer};`;实际测试比本稿更完整(7 个用例,含标点过滤、稠密位置、与 `chinese_compatible` 的差异对比)。

```rust
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
/// Unlike `chinese_compatible`, which splits every Han character into its own
/// token, this tokenizer consults the `jieba` dictionary so that
/// multi-character words such as `天气` stay a single token.
///
/// It runs jieba in *search engine mode* (`cut_for_search`), which additionally
/// emits the sub-words contained in a long word. That is what keeps phrase
/// queries working when the segmentation of the query differs from the
/// segmentation of the indexed text.
#[derive(Clone, Default)]
pub(crate) struct ChineseDictTokenizer;

/// The bundled dictionary is several megabytes and takes a non-trivial amount
/// of time to load, so it is built once per process rather than per document.
static JIEBA: LazyLock<Jieba> = LazyLock::new(Jieba::new);

/// A token is searchable if it carries at least one alphanumeric character.
///
/// This mirrors `chinese_compatible`, whose `Grouping::SplitIgnore` drops every
/// non-alphanumeric character: punctuation-only and whitespace-only tokens must
/// never reach the inverted index.
fn is_searchable(text: &str) -> bool {
    text.chars().any(char::is_alphanumeric)
}

pub(crate) struct ChineseDictTokenStream<'a> {
    text: &'a str,
    /// Byte ranges of the tokens that survive filtering, in jieba's output order.
    ///
    /// Ranges can overlap and are not monotonically increasing: in search engine
    /// mode jieba emits the sub-words before the long word containing them.
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
        // `hmm = true` lets jieba recover words that are absent from the
        // dictionary (new words, product names, error codes).
        //
        // Hidden contract: positions are assigned by enumeration order, which is
        // strictly increasing. They must never decrease — tantivy encodes
        // positional postings as deltas. Do NOT switch to byte offsets as
        // positions: with `cut_for_search` the offsets overlap and are not
        // monotonic.
        //
        // Filtering happens before position assignment, so a dropped token
        // consumes no position. Both the index side and the query side use this
        // same tokenizer, so phrase queries stay aligned.
        let tokens = JIEBA
            .cut_for_search(text, true)
            .into_iter()
            .filter(|token| is_searchable(token.word))
            // `byte_start`/`byte_end` are byte offsets. `start`/`end` are char
            // indices and cannot slice `text`.
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

    fn tokens_of(text: &str) -> Vec<String> {
        let mut tokenizer = super::ChineseDictTokenizer;
        let mut stream = tokenizer.token_stream(text);
        let mut tokens = Vec::new();
        while stream.advance() {
            tokens.push(stream.token().text.clone());
        }
        tokens
    }

    #[test]
    fn test_chinese_dict_tokenizer_keeps_words() {
        let tokens = tokens_of("今天天气很好, hello world");
        // The whole point of this tokenizer: a dictionary word stays one token.
        assert!(tokens.contains(&"天气".to_string()), "tokens={tokens:?}");
        // Non-Han text must survive unchanged.
        assert!(tokens.contains(&"hello".to_string()), "tokens={tokens:?}");
    }

    #[test]
    fn test_chinese_dict_tokenizer_drops_punctuation() {
        let tokens = tokens_of("你好，世界！code=500");
        // A token made only of punctuation or whitespace must not be indexed.
        assert!(
            tokens.iter().all(|token| token.chars().any(char::is_alphanumeric)),
            "tokens={tokens:?}"
        );
        for punctuation in ["，", ",", "!", "="] {
            assert!(
                !tokens.contains(&punctuation.to_string()),
                "tokens={tokens:?}"
            );
        }
        assert!(tokens.contains(&"code".to_string()), "tokens={tokens:?}");
        assert!(tokens.contains(&"500".to_string()), "tokens={tokens:?}");
        assert_eq!(tokens_of("，。！,"), Vec::<String>::new());
    }

    #[test]
    fn test_search_mode_emits_subwords_on_consecutive_positions() {
        // This is the property that makes phrase queries survive
        // index/query segmentation mismatch.
        let indexed = tokens_of("毕业于中国科学院计算所");
        let queried = tokens_of("中国科学院");
        let start = indexed
            .windows(queried.len())
            .position(|window| window == queried.as_slice());
        assert!(start.is_some(), "indexed={indexed:?} queried={queried:?}");
    }

    #[test]
    fn test_offsets_point_back_into_the_original_text() {
        let text = "错误 emoji 🚀 code=500";
        let mut tokenizer = super::ChineseDictTokenizer;
        let mut stream = tokenizer.token_stream(text);
        while stream.advance() {
            let token = stream.token();
            assert!(text.is_char_boundary(token.offset_from));
            assert!(text.is_char_boundary(token.offset_to));
            assert_eq!(&text[token.offset_from..token.offset_to], token.text);
        }
    }
}
```

**设计说明**

- **`LazyLock<Jieba>` 静态实例**:`Jieba` 持有整本词典,构造昂贵。放在 `static` 里保证每个进程只构造一次;放在 `ChineseDictTokenizer` 结构体里会导致每建一个 `TextAnalyzer` 都重建词典。
- **保存字节区间而非借用 jieba 的 `Token`**:`cut_for_search` 返回的 token 带着输入串的生命周期,存进流结构体会把生命周期耦合变复杂;存 `(usize, usize)` 字节区间、再从 `text` 切片更简单,也更接近仓库 `unicode_segmenter_tokenizer.rs` 的既有写法。
- **`position` 单调递增(隐藏契约,必须保持)**:位置按枚举序号赋值。tantivy 的位置倒排按增量编码,**非单调位置会破坏索引**;`cut_for_search` 的偏移区间既重叠又非单调,所以绝不能用偏移当位置。此约束需在 pin 住的 tantivy revision 上再确认一次。
- **过滤发生在位置赋值之前**:被丢弃的 token 不占位置。索引侧与查询侧使用同一实现,短语语义仍然对齐。
- **`offset_from` / `offset_to`**:取 jieba 返回的字节偏移,保证高亮能定位原文。**注意 D2 带来的副作用**:偏移区间可能重叠且非单调,高亮是否会出现重叠片段需实测(见第 9 节)。
- **`position_length` 显式写 1**:每个 token 占一个位置,这是词典分词的常规语义。

### 4.4 修改 `quickwit/quickwit-query/src/tokenizers/mod.rs`

**改动一:模块声明**(`mod.rs:15-18` 的 `mod` 列表)

```rust
mod chinese_compatible;
mod chinese_dict;          // 新增
mod code_tokenizer;
mod tokenizer_manager;
mod unicode_segmenter_tokenizer;
```

**改动二:导入**(`mod.rs:27` 附近)

```rust
use self::chinese_compatible::ChineseTokenizer;
use self::chinese_dict::ChineseDictTokenizer;   // 新增
```

**改动三:注册**(紧跟 `mod.rs:66` 的 `chinese_compatible` 之后)

```rust
let chinese_dict_tokenizer = TextAnalyzer::builder(ChineseDictTokenizer)
    .filter(RemoveLongFilter::limit(DEFAULT_REMOVE_TOKEN_LENGTH))
    .filter(LowerCaser)
    .build();
tokenizer_manager.register("chinese_dict", chinese_dict_tokenizer, true);
```

参数说明:第三个参数 `true` 表示该分词器做小写化——它决定该字段的 `normalizer` 走 `raw_lowercase` 还是 `raw`(`tokenizer_manager.rs:64-87`)。`chinese_compatible` 传的也是 `true`,保持一致。

**这一步是"让它可配置"的全部关键**,理由不只是"名字按名解析",而是**两条生产路径共用同一个工厂函数**:

| 路径 | 位置 | 说明 |
|---|---|---|
| 索引配置校验 + 索引侧 | `doc_mapper_impl.rs:205` | 自定义 tokenizer 在这个管理器上叠加注册 |
| 查询分析侧 | `query_ast/mod.rs:198` | 进程级 `LazyLock<TokenizerManager>`,同样调 `create_default_quickwit_tokenizer_manager()` |

两者都走 `create_default_quickwit_tokenizer_manager()`,所以在此处注册即可同时覆盖,入口只有一个。**无需改动 `TokenizerType` 枚举、JSON Schema 或 OpenAPI 定义。**

> **注意内置名是保留的**:`doc_mapper_impl.rs:213-222` 会拒绝与内置分词器重名的自定义 tokenizer。新增 `chinese_dict` 后,用户在 `tokenizers:` 段里不能再定义同名项——这是预期行为。

### 4.5 无需改动的部分(已核实)

- **wildcard / regex 查询**:`sub_query_parts_to_regex` 走的是 `tokenizer_manager.get_normalizer(tokenizer_name)`(`tokenizer_manager.rs:79`),即 `raw` / `raw_lowercase`,与字段的 tokenizer 实现无关。只要注册时 `does_lowercasing = true`,`get_normalizer("chinese_dict")` 就会正确解析到 `raw_lowercase`,无需在 `wildcard_query.rs` 的测试枚举里额外做特殊处理(该枚举只是覆盖各 tokenizer 名,新增项与之同类)。
- **ES 兼容层 / REST 层 / proto**:分词器名是自由字符串,没有任何枚举校验。

### 4.6 修改 `docs/configuration/index-config.md`(初版遗漏的交付项)

新增一个可配置分词器属于**对外可见行为**,按仓库规范必须同步补文档:

- 分词器表(`docs/configuration/index-config.md:140-149`)新增 `chinese_dict` 一行,写明:词典分词、搜索引擎模式、丢弃纯标点 token、建议配 `record: position`
- JSON 字段的 `tokenizer` 说明(`:377`)目前写的是 `choices between raw, default, en_stem and chinese_compatible`,需一并修正(该处本身就含有失效项 `en_stem`,见第 11 节)

### 4.7 修改依赖

**`quickwit/Cargo.toml`**(`[workspace.dependencies]`,按字母序插入)

```toml
jieba-rs = "0.11"
```

**`quickwit/quickwit-query/Cargo.toml`**(`[dependencies]`)

```toml
jieba-rs = { workspace = true }
```

遵循仓库规范:优先引用 workspace 依赖,不在成员 crate 里直接写版本。`quickwit-query` 目前**没有 `[features]` 段**,本方案也不需要新增。

### 4.8 为什么不加 feature gate(重要架构判断)

**分词器绝不能 feature-gate。** 原因:分词器名是写进索引元数据的。如果 indexer 编译时带了 `chinese_dict` 而某个 searcher 没带,那个 searcher 在加载该索引配置时会直接失败:

```text
unknown tokenizer `chinese_dict` for field `body`
```

这会导致集群节点行为分裂。因此本方案**无条件注册**(与 `chinese_compatible` 一致)。

**但"注册"不等于"加载"**(修订):`TextAnalyzer::builder(ChineseDictTokenizer)` 只构造 analyzer,那 32 MiB 的词典要等第一次真正调用 `token_stream` 时才由 `LazyLock` 加载(实测见 13.6)。因此:

- 纯 searcher 加载含 `chinese_dict` 的索引配置时,`validate_fields_tokenizers` 只做名字查表,**不会**触发加载;
- 只有"实际用该分词器分过词"的进程才支付这 32 MiB。

所以内存成本比初版描述的"所有服务都带上词典"要小。代价是**加载后不可卸载**,且实例不共享(再 new 一个实例额外 +24 MiB)。

> 这也侧面解释了上游为什么最终删掉 `multilang`:任何会影响 schema 解析的 feature gate 都是运维陷阱。

**同一个约束在滚动升级时更严重**(初版未写):一旦有索引使用了 `chinese_dict`,**任何尚未升级、不认识该名字的 searcher/leaf 都会在加载该索引配置时启动失败**。因此实际运维约束是"**全部节点升级完成之前,不得创建使用 `chinese_dict` 的索引**";而且一旦创建,**没有回滚路径**(改回旧 tokenizer 不会让已写入的索引重新分词,只能改配置后重建)。这个时序约束比内存开销重要得多,必须写进发布计划。

---

## 5. 使用方式

### 5.1 索引配置

```yaml
version: 0.8
index_id: cn-logs
doc_mapping:
  field_mappings:
    - name: body
      type: text
      tokenizer: chinese_dict
      record: position        # 关键,见 5.2
      indexed: true
  default_search_fields: [body]
```

JSON 字段同样支持(`docs/configuration/index-config.md:377` 的 `tokenizer` 选项)。

### 5.2 必须配 `record: position`

`record` 的默认值是 `basic`(只记录 DocId,见 `docs/configuration/index-config.md:136`)。三个选项:

| 值 | 记录内容 |
|---|---|
| `basic`(默认) | 仅 DocId |
| `freq` | DocId + 词频 |
| `position` | DocId + 词频 + **位置** |

词典分词要做词组/短语查询(如 `"中国科学院"`),必须有位置信息;否则只能做单词匹配。`docs/configuration/index-config.md:149` 对 `chinese_compatible` 也强调了这一点。

**D2 使这一条从"建议"变成"强制"**:`cut_for_search` 的收益(子词与整词按连续位置排布)只有在记录位置时才能体现。

### 5.3 建索引与写入

```bash
# 建索引
quickwit index create --index-config cn-logs.yaml

# 写入
curl -XPOST http://localhost:7280/api/v1/cn-logs/ingest \
  -H 'Content-Type: application/json' \
  -d '{"body":"今天天气很好"}'
```

### 5.4 查询验证

```bash
# 单词语义:应能命中,因为 "天气" 是索引中的一个完整 token
curl -s http://localhost:7280/api/v1/cn-logs/search \
  -H 'Content-Type: application/json' \
  -d '{"query":"天气","search_fields":["body"]}'

# 短语语义:验证位置信息生效
curl -s http://localhost:7280/api/v1/cn-logs/search \
  -H 'Content-Type: application/json' \
  -d '{"query":"\"今天 天气\"","search_fields":["body"]}'
```

---

## 6. 测试计划

**核心原则**:词典方案的收益就是"词"的语义与"子词召回",因此断言必须**能区分词典分词与逐字分词**。仅断言 `contains("天气")` 这类"存在性"检查对"多出垃圾 token""位置错乱"完全不敏感,属于自我欺骗型测试——这正是第 11 节批评 `en_stem` 时的同类错误,不能在本方案重演。

| 层级 | 内容 | 位置 |
|---|---|---|
| 单元测试 | 词典词保持单 token(`天气`);非汉字文本不丢(`hello`) | `chinese_dict.rs` |
| 单元测试 | **不得产出纯标点/空白 token**:`，。！,` 输入得到空序列;所有输出 token 至少含一个字母数字字符 | `chinese_dict.rs`(已给) |
| 单元测试 | **偏移合法性**:`is_char_boundary` 成立且 `text[offset_from..offset_to] == token.text` | `chinese_dict.rs`(已给) |
| 单元测试 | **位置单调性**:position 严格递增,无空洞 | `chinese_dict.rs` |
| 单元测试 | **子词连续性(方案核心收益)**:索引串 `毕业于中国科学院计算所` 的 token 序列包含查询串 `中国科学院` 的 token 序列且位置连续 | `chinese_dict.rs`(已给) |
| 单元测试 | 与 `chinese_compatible` 的差异断言:同一输入 token 数不同、且 `天气` 在 `chinese_compatible` 下被拆为 2 个 token | `chinese_dict.rs` |
| 单元测试 | 注册可见性:`create_default_quickwit_tokenizer_manager().get_tokenizer("chinese_dict")` 为 `Some` | `tokenizers/mod.rs` 的 `test_tokenizers_in_manager` |
| 基准测试 | 索引吞吐 / token 数对比 `chinese_compatible`(D2 会显著增加 token 数,需量化索引膨胀) | `benches/tokenizers_bench.rs` |
| 集成测试 | **必须走生产路径**:建索引 → REST `/ingest` 写入 → REST `/search` 查询 → 校验命中、高亮、短语 | `quickwit-integration-tests` |
| 兼容性测试 | 修改 tokenizer 必须重建索引,验证旧索引不受影响 | 手工/集成 |

**验收必须可判定**(初版只写了"校验命中与高亮",无法判定)。以下验收集**已在真实 REST 链路上跑过**(见第 13.5 节),并据结果修正了初版的两处错误期望:

1. `tokenizer: chinese_dict` + `record: position`,写入 `{"body":"今天天气很好"}`:
   - 查询 `天气` → **必须命中** ✅
   - 查询 `"今天天气"`(整词,无空格)→ **必须命中** ✅
   - ~~查询 `"今天 天气"` → 必须命中~~ ❌ **初版此条错误**:搜索引擎模式在 `今天` 与 `天气` 之间插入了子词 `天天`(位置 1),索引序列为 `今天`0/`天天`1/`天气`2/`今天天气`3,而带空格的短语查询只会产出 `[今天, 天气]` 两个 token,要求相邻位置因而不成立。**这是 D2 的固有代价,必须记为已知行为**。
2. 写入 `{"body":"今天很气人"}`(**真正含 `天` 与 `气` 且不相邻**):
   - 查询 `天气` → **必须不命中该文档** ✅(实测只命中 `今天天气很好`)。这才是区分词典分词与逐字分词的用例;初版写的 `今天很热` 并不含 `气`,逐字分词同样不会命中,没有区分力。
3. 写入 `{"body":"毕业于中国科学院计算所"}`:
   - 查询 `中国科学院` → **必须命中**(验证 D2 的子词收益)。单测已覆盖 token 序列连续性;REST 侧未单独跑。
4. 写入含 `，`、`!` 的文本:确认高亮片段不包含标点、且不出现重叠片段。**未验证**。

> 仓库规范明确:**未经过 HTTP/gRPC 真实链路验证,不得声称功能可用**。单元测试通过不等于功能可用。

---

## 7. 提交前闸门

```bash
cd quickwit

# 依赖与许可(新增依赖后必做)
make update-licenses                # dd-rust-license-tool 刷新 LICENSE-3rdparty.csv
cargo machete                       # 依赖是否真被使用
cargo deny check licenses           # ⚠️ 先确认 jieba-rs 声明了 SPDX license,见 4.1

# 代码质量
cargo +nightly fmt --all
cargo clippy --workspace --all-features --tests
bash scripts/check_license_headers.sh

# 文档构建(确认未破坏)
make doc
```

**许可证头**:本方案新增的 `.rs` 文件必须带 Apache 2.0 头(已在 4.3 的代码中给出)。许可证头检查脚本只扫描路径中含 `build|src|proto` 且扩展名为 `.rs/.proto/.ts` 的文件,`src/` 下的新文件在检查范围内。

### 本机环境现状(2026-10-14 复核,初版结论需修正)

| 项 | 初版说法 | 实际(2026-10-14 实施时) |
|---|---|---|
| Rust 工具链 | "无可用工具链(1.96 未安装)" | `rustc`/`cargo` 1.99.0 可用;`rustup` 可安装 pin 的 `1.96.1`;`rustup toolchain install nightly` 亦成功 |
| `protoc` | "无 protoc(9 个 build.rs 依赖)" | **`protoc 3.14.0` 可用**。且只有 `quickwit-proto/build.rs` 依赖它,`quickwit-query` / `quickwit-common` / `quickwit-datetime` 都不直接依赖 `quickwit-proto`,故 `cargo test -p quickwit-query` 本就不需要 `protoc` |
| Docker | "daemon 未运行" | **daemon 可用**,`make test-all` 具备前提 |
| 网络 | 未提及 | 可用:`static.crates.io` 的 `.crate` 归档(4.8 MB)2.4 s 下载成功。此前"下载挂住"是阶段性问题,不是环境限制 |

**结论**:第 4、6 节的单测级验证已完成(见第 13 节)。`--all-features` 全量构建与 `make test-all` 级别的验证仍待执行。

---

## 8. 风险与缓解

| 风险 | 影响 | 缓解 |
|---|---|---|
| **许可声明未确认** | `cargo deny check licenses` / `make update-licenses` 直接失败 | **第一步就确认 `jieba-rs 0.11.0` 的 SPDX 声明**;若确实缺失,评估 `cang-jie` 或自带词典文件分发 |
| **必须重建索引** | 分词器在建索引时固化进 schema,换分词器后旧 split 仍是旧分词,无 in-place 变更 | 规划双写/切换窗口;或用新 `index_id` 重建后切流量 |
| **滚动升级时序** | 未升级的 searcher 加载含 `chinese_dict` 的索引配置会直接失败,**无回滚路径** | 全部节点升级完成前不得创建此类索引;写进发布计划 |
| **D2 导致索引膨胀** | `cut_for_search` 输出 token 数明显多于精确模式 | 用基准测试量化;必要时收缩到精确模式 + 应用侧改写查询 |
| **偏移重叠/非单调** | 高亮可能出现重叠或乱序片段 | 实测高亮输出;必要时在流内对偏移做排序/去重,或高亮只取最长区间 |
| **位置必须单调(硬约束)** | 违反会破坏位置倒排(增量编码) | 保持"枚举序号赋值";在 pin 的 tantivy revision 上确认约束 |
| **词典覆盖不全** | 专有名词、产品名、错误码被 HMM 兜底切错,召回/精度不稳定 | 二期增加用户自定义词典(第 10 节);先收集真实日志验证切分效果 |
| **内存与冷启动** | 词典 RSS **32 MiB**(实测),加载 **109 ms**;实例不共享(第二个 +24 MiB) | 只在实际用该分词器分过词的进程里支付(见 4.8、13.6)。压内存需改用 `Jieba::with_dict()` 裁剪词表,代价是分词质量 |
| **Lambda 冷启动** | `quickwit-lambda-server` 作为 leaf 搜索,首次分词触发 **32 MiB / 109 ms** 的加载,放大冷启动 | 数字见 13.6;注意"注册 ≠ 加载",没查到中文就不付这个成本;必要时预热 |
| **分词性能** | 实测 **1.79 µs/文档** → 不是瓶颈 | **无需优化**。已实测否决两条看似可行的改动(存 `Token` 代替字节区间、ASCII 快速路径),且 jieba 公开 API 无惰性迭代器(13.6)。索引路径调用点在 `quickwit-indexing/src/actors/indexer.rs:341` |
| **单字查询漏召** | 长词整体成 token 后不再拆单字,查单字不命中(实测 `气` 查不到 `今天天气很好`) | 在用户文档中写明(4.6、13.7);若要坚持单字召回需索引期额外写入单字 token,与 D2 的初衷冲突 |
| **依赖 API 变动** | `0.11` 与初版假设的 `0.7` 之间 API/feature 已变 | 锁定 `0.11` 并实测确认签名;用单元测试兜住 |
| **文档误导** | 官方文档与 `AGENTS.md` 中存在已失效的分词器说明(第 11 节) | 落地时同步修正,并补 4.6 的新增文档 |

---

## 9. 待确认清单核对结果(实施后回填)

| # | 待确认项 | 结果 |
|---|---|---|
| 1 | `jieba-rs 0.11.0` 的 SPDX 许可声明 | **已确认**:crate manifest 声明 `license = "MIT"` |
| 2 | `Jieba::new()` 是否免参、`cut_for_search` 签名与返回字段名 | **已确认**:`new()` 免参;`cut_for_search(&self, &'a str, bool) -> Vec<Token<'a>>`;字段含义见 4.1 |
| 3 | `Jieba: Send + Sync`(静态实例的前提) | **已确认**:`assert_send_sync::<Jieba>()` 通过,`LazyLock` 静态实例成立 |
| 4 | jieba 对标点的确切输出,D3 过滤规则是否足够 | **已确认**:空白、标点、emoji 各成独立 token,`is_searchable` 规则足够。**附带发现**:`95%`、`WES-5.4.5`、`k8s.node.id` 会被 jieba 当作**整体 token**(它把 `.`/`_`/`-`/`%` 视作连接符),与 `chinese_compatible` 逐字符切分不同;因为查询串走同一分词器,所以自洽。**遗留影响**:按词查询 `95` 匹配不到 `95%` |
| 5 | `cut_for_search` 输出顺序与偏移重叠的确切形态 | **已确认**:先子词、后整词;偏移重叠且非单调(如 `服务`0..6 → `务器`3..9 → `服务器`0..9) |
| 6 | tantivy 对位置单调性的硬性要求 | **已确认须保持单调**。该 pin revision 的 `Token` 为 `{ offset_from: usize, offset_to: usize, position: usize, position_length: usize }`——注意 **`position` 是 `usize` 而非 `u32`** |
| 7 | 高亮路径在偏移重叠时的输出 | **未做**,需集成测试 |
| 8 | 词典加载时间与常驻内存 | **已确认**:加载 **109 ms**(release;初版记的 482 ms 是 debug 构建的数字),常驻 **32 MiB**,第二个实例再 +24 MiB(即实例不共享)。详见 13.6 |

---

## 10. 后续增强(不在本期范围)

1. **用户自定义词典**:允许索引配置声明附加词表(如 `tokenizers: [{name: cn, type: dictionary, user_dict: ...}]`),支持业务专有词;这会引入"词典文件需在每个节点可用"的分发问题,以及 `TokenizerType` 的 Schema 变更。
2. **同义词与停用词**:日志场景常见的 `ERROR`/`错误` 归一化需求。
3. **繁简归一**:`cang-jie` 或前置归一化过滤器。
4. **日韩支持**:当前 `chinese_compatible` 与 `jieba` 均不覆盖假名(`3040–30FF`)与韩文音节(`AC00–D7AF`);若需 CJK 全支持,需另行评估 lindera(方案 C)。
5. **分词效果评测集**:建立一批真实中文日志样本 + 期望 token 序列,作为回归基线。

---

## 11. 顺带发现:文档与实际代码不一致

落地本方案时建议一并修正,否则会误导使用者:

1. **`en_stem` 分词器在文档中存在但代码中不存在**(已核实,成立)
   - 文档记载:`docs/configuration/index-config.md:147`、`:377`
   - 实际:HEAD 上**没有任何地方注册** `en_stem`(10 个内置分词器中无此项)
   - 后果:配置 `tokenizer: en_stem` 会报 `unknown tokenizer`
   - 唯一出现它的代码是 `field_mapping_entry.rs:1194`,那是个仅做反序列化、不做名字校验的测试,所以测试通过掩盖了问题

2. **`multilang` 被 `AGENTS.md` 描述为 feature gate,但代码中不存在**(已核实,成立;**引用位置初版写错了**)
   - 实际记载于 `AGENTS.md:94`(初版误写为 `quickwit/CLAUDE.md`;根目录 `CLAUDE.md` 只有 11 字节,是指向 `AGENTS.md` 的转发说明)
   - 实际:`Cargo.toml` 与所有 `.rs` 文件中零命中

3. **`chinese_compatible` 的分词粒度未在文档中说明**(已核实,成立)
   - `docs/configuration/index-config.md:149` 只写了 "Chop between each CJK character",未说明它是**逐字切分、无词典**,使用者容易误以为它等同于词典分词

---

## 12. 附:已核实事实表

全部为代码内可定位的静态事实(行号按 HEAD `a73912ae` 复核):

| 事实 | 出处 |
|---|---|
| `TokenizerType` 仅 4 个变体 | `quickwit-doc-mapper/src/doc_mapper/tokenizer_entry.rs:118` |
| `TokenFilterType` 仅 3 个变体 | `quickwit-doc-mapper/src/doc_mapper/tokenizer_entry.rs:90` |
| 未注册分词器会被拒绝 | `quickwit-doc-mapper/src/doc_mapper/doc_mapper_impl.rs:357-379`(消息在 `:375`) |
| 内置名保留、自定义不得重名 | `quickwit-doc-mapper/src/doc_mapper/doc_mapper_impl.rs:213-222` |
| 索引侧管理器来自同一工厂 | `quickwit-doc-mapper/src/doc_mapper/doc_mapper_impl.rs:205` |
| 查询分析侧管理器来自同一工厂 | `quickwit-query/src/query_ast/mod.rs:198` |
| 10 个已注册分词器 | `quickwit-query/src/tokenizers/mod.rs:36-94` |
| `chinese_compatible` 注册位置 | `quickwit-query/src/tokenizers/mod.rs:66` |
| `register` 第三参 = 是否小写化 | `quickwit-query/src/tokenizers/tokenizer_manager.rs:64-87`(含 `:79` 的 `get_normalizer`) |
| `chinese_compatible` 逐字切分行为 | `quickwit-query/src/tokenizers/chinese_compatible.rs:65-113`(CJK 区段在 `:51`),单测 `:131` |
| 自定义 tokenizer 实现模板 | `quickwit-query/src/tokenizers/unicode_segmenter_tokenizer.rs`(106 行) |
| `MIT` 在许可允许列表 | `quickwit/deny.toml:65` |
| `jieba-rs 0.11.0` 的许可 / feature / API / `Token` 字段 | 解包 `jieba-rs-0.11.0.crate` 后的 `Cargo.toml` 与 `src/lib.rs` |
| tantivy(pin rev)的 `Token` 字段类型 | `~/.cargo/git/checkouts/tantivy-9cf9af06161d52ce/2e87c2e/tokenizer-api/src/lib.rs:15-29` |
| lindera CC-CEDICT 历史依赖 | `quickwit/dependency-licenses.html:14539` |
| `multilang` 移除记录 | `CHANGELOG.md:38-39`(另见 `:178-179`) |
| `record` 默认 `basic` | `docs/configuration/index-config.md:136` |
| 分词器文档表 / JSON `tokenizer` 说明 | `docs/configuration/index-config.md:140-149`、`:377` |
| `multilang` feature gates 记载位置 | `AGENTS.md:94` |

**验证声明**:本文档 4.3 节的代码已落地为 `quickwit/quickwit-query/src/tokenizers/chinese_dict.rs`,并通过 `cargo test -p quickwit-query --lib`(192 个测试全绿,含 7 个新增用例与 2 个管理器注册用例)。**已手工验证** REST 端到端链路(13.5),**已实测**性能与内存(13.6)。**尚未验证**:高亮在偏移重叠时的行为、`--all-features` 全量构建、自动化集成测试套件。详见第 9、13 节。

---

## 13. 实现状态(2026-10-14)

### 13.1 已落地的改动

| # | 文件 | 改动 |
|---|---|---|
| 1 | `quickwit/quickwit-query/src/tokenizers/chinese_dict.rs` | 新增,262 行(含 7 个单元测试) |
| 2 | `quickwit/quickwit-query/src/tokenizers/mod.rs` | 模块声明、导入、注册 `chinese_dict`;`test_tokenizers_in_manager` 增补;新增 `test_chinese_dict_is_registered_as_a_lowercaser` |
| 3 | `quickwit/quickwit-query/Cargo.toml` | `jieba-rs = { workspace = true }` |
| 4 | `quickwit/Cargo.toml` | `[workspace.dependencies]` 增加 `jieba-rs = "0.11"` |
| 5 | `docs/configuration/index-config.md` | 分词器表新增 `chinese_dict`;修正 JSON `tokenizer` 字段那条失效枚举(原文列了不存在的 `en_stem`) |

### 13.2 验证结果

```text
cargo test -p quickwit-query --lib
test result: ok. 192 passed; 0 failed; 0 ignored        (16.17s)

cargo test -p quickwit-query --lib chinese_dict
test result: ok. 8 passed; 0 failed; 184 filtered out   (1.18s,增量)

cargo clippy -p quickwit-query --all-targets --all-features   → 无告警
cargo machete                                                 → 无未使用依赖
cargo deny check licenses                                     → licenses ok
cargo +nightly fmt -p quickwit-query --check                  → 通过
check_license_headers.sh / check_log_format.sh                → 通过
make update-licenses                                          → LICENSE-3rdparty.csv 恰好 +13 行
```

**关于 `Cargo.lock`**:改动为 148 增 / 23 删。其中 **13 个新包**(`jieba-rs`、`jieba-macros` 及其传递依赖);另有 23 行是对既有包依赖边的**重指向**(如 `anstyle-query`、`colored` 的 `windows-sys` 由 0.48/0.60 指向 0.61.2,`error-chain` 的 `socket2` 0.5→0.6,`oauth2` 的 `base64` 0.21→0.22),**没有任何包版本被改动、也没有包被移除**。这是 cargo 在加入新依赖时重新解析全图所做的收敛;`cargo metadata --locked` 接受该结果。如需进一步压缩 diff,需人工改写 lockfile,不推荐。

### 13.3 实测行为(作为测试期望值的依据)

| 输入 | `chinese_dict` 输出(过滤后) |
|---|---|
| `今天天气很好, hello world` | `今天 / 天天 / 天气 / 今天天气 / 很 / 好 / hello / world` |
| `毕业于中国科学院计算所` | `毕业 / 于 / 中国 / 科学 / 学院 / 科学院 / 中国科学院 / 计算 / 计算所` |
| `你好，世界！code=500 🚀` | `你好 / 世界 / code / 500`(标点、空白、emoji 全部丢弃) |
| `你好世界` | `你好 / 世界`(对照:`chinese_compatible` 为 `你 / 好 / 世 / 界`) |
| 单字 `气` | ⚠️ **查不到** `今天天气很好`(token 里只有 `天气`/`今天天气`,没有单字 `气`)。见 13.7 |

### 13.4 尚未完成

1. **workspace 级** clippy(`cargo clippy --workspace --all-features --tests`)未跑:目前只跑了 `-p quickwit-query --all-targets --all-features`。全量 `--all-features` 构建与 `make test-all`(需 Docker 服务)同样未跑。
2. **自动化**集成测试(`quickwit-integration-tests` 套件)未跑。手工 REST 链路已验证,见 13.5。
3. 基准测试:`benches/tokenizers_bench.rs` 中量化 D2 带来的 token 数增长与索引膨胀。
4. 高亮在偏移重叠时的输出(第 6 节验收用例 4)未验证。
5. 第 11 节的 3 处既有文档不一致(`en_stem` 表行、`AGENTS.md:94` 的 `multilang`、`chinese_compatible` 粒度说明)尚未处理——属既有问题,未纳入本次改动范围。

### 13.5 手工 REST 链路验证(2026-10-14)

用源码构建的二进制(`cargo build -p quickwit-cli`,debug)单机启动,走完整生产链路:

```bash
# 数据目录必须先存在,Quickwit 不会自动创建
mkdir -p /tmp/qw-run/qwdata && cd /tmp/qw-run
/opt/quickwit/quickwit/target/debug/quickwit run \
    --config /opt/quickwit/config/quickwit.yaml -y

# 建索引(tokenizer: chinese_dict)、写入、查询
curl -XPOST localhost:7280/api/v1/indexes -H 'content-type: application/yaml' \
     --data-binary @cn-logs.yaml
curl -XPOST 'localhost:7280/api/v1/cn-logs/ingest?commit=force' \
     -H 'content-type: application/json' -d '{"body":"今天天气很好"}'
curl 'localhost:7280/api/v1/cn-logs/search?query=天气'
```

结果:

| 检查项 | 结果 |
|---|---|
| 索引配置含 `tokenizer: chinese_dict` 能否创建 | ✅ 成功,返回的 index config 里 `"tokenizer": "chinese_dict"` |
| 写入 `今天天气很好` 后查询 `天气` | ✅ `num_hits: 1` |
| 写入 `今天很气人`(含 `天` 与 `气`,不相邻)后查询 `天气` | ✅ `num_hits: 1`,**只命中 `今天天气很好`**,未误命中 |
| 短语查询 `"今天天气"` | ✅ `num_hits: 1` |
| 短语查询 `"今天 天气"`(带空格) | ❌ `num_hits: 0` —— D2 的固有代价,见第 6 节 |
| ES 兼容 API `GET /api/v1/_elastic/cn-logs/_search?q=天气` | ✅ 命中同一文档 |
| `GET /openapi.json` | ✅ 200 |

---

### 13.6 实测性能与内存(release 构建)

数字来自 `/tmp/jieba-check`(只依赖 `jieba-rs`,**不含 tantivy 与索引流水线**),4 篇真实中文日志样本,20 000 轮。绝对值不能直接外推到真实索引吞吐,比例关系可用。

| 指标 | 实测值 |
|---|---|
| 分词成本(`cut_for_search` + 过滤 + collect) | **1.79 µs/文档**;其中 jieba 本体 1.41 µs → 单核约 **55 万文档/秒** |
| 精确模式 `cut` 的成本 | 1.08 µs/文档 → 搜索模式比精确模式贵约 **31%** |
| 词典加载(`Jieba::new()`) | **109 ms**(release);初版记的 482 ms 是 debug 的数字 |
| 词典常驻内存(RSS 增量) | **32 MiB**;第二个实例再 +24 MiB → **实例各自一份,不共享** |
| token 膨胀(`cut_for_search` / `cut`) | 1.00×–2.00×(均值约 1.4×) |
| 标点过滤丢弃比例 | 0%–45%(空格、`=`、`，` 各占一个 token) |

**两个被实测否决的"优化"**(记录在此,后人不必再试):

1. 把 `Vec<(usize, usize)>` 换成 `Vec<jieba_rs::Token>`(想省掉第二次分配)会**更慢**:1.839 vs 1.791 µs/文档 —— 元素从 16 B 变 48 B,移动成本盖过省下的那次分配。**当前"存字节区间"的设计是更优解。**
2. 给 `is_searchable` 加 ASCII 快速路径会**更慢**:汉字 token 的 ASCII 判定必然失败,还要再跑一遍 `chars()`,整体负收益。

**一处必须纠正的旧结论**:本文档早期版本把"改造为惰性迭代器(jieba 提供 `tokenize()`)"列为性能缓解措施。**这是错的**——`cut` / `cut_for_search` / `tokenize` 在 jieba 0.11 中**全部返回 `Vec`**,公开 API 没有惰性迭代器(`cut_each` 是私有的)。在现有 API 下无法彻底省掉那次分配。

**结论**:分词 CPU 不是瓶颈,热路径没有值得做的优化。真正的成本是 **32 MiB 常驻 + 109 ms 加载**,以及下面这条召回缺口。

### 13.7 已知召回缺口:单字查询

`今天天气很好` 索引出的 token 是 `今天 / 天天 / 天气 / 今天天气 / 很 / 好`,**其中没有单字 `气`**。因此:

- 查询 `气`(单字)→ **不命中**;`chinese_compatible` 下会命中
- 查询 `天气`(整词)→ 命中

这是词典分词的必然代价:长词整体成 token 后就不再拆回单字。**对"在日志里搜单个汉字"的用户是可见的召回下降**,已在 `docs/configuration/index-config.md` 的 `chinese_dict` 说明中向用户写明。
