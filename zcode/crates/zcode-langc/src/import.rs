//! `.sublime-syntax`（YAML）→ `language.toml` の下書き。
//!
//! 変換の方針:
//! - 変数（`{{name}}`）は全部展開してから正規表現を直す（Oniguruma の書き方が部品に
//!   隠れていても見つけられるように）
//! - スコープ名（`keyword.control.python` など）は、最も近い [`TokenKind`] に寄せる
//! - `captures` は区間ごとに色を分けられないので、区間全体に 1 色（警告）
//! - `meta_scope` / `meta_content_scope` は文脈の地のトークンにする
//! - 名前の無い文脈（push の中に直接書いた規則の列）は `<親>__<番号>` の名前を付ける
//! - `prototype` は各文脈の先頭に `include` する（`meta_include_prototype: false` を除く）
//! - `embed` / `escape`・ほかの構文の取り込み（`scope:..`）・`branch_point` は変換しない（エラー）
//! - 末尾の先読み `(?=..)` は `followed_by`、先読みだけの規則は空の `match` + `followed_by`

use crate::regex::convert;
use std::collections::BTreeMap;
use yaml_rust::{Yaml, YamlLoader};
use zc_core::TokenKind;
use zc_highlight::compile::{check_regex, compile_language};
use zc_highlight::schema::{LanguageFile, OneOrMany, PopSpec, Rule};

/// 変換の報告。
#[derive(Debug, Default, Clone)]
pub struct Report {
    /// 近い形にした規則（`文脈[番号]: 内容`）。
    pub warnings: Vec<String>,
    /// 変換できずに外した規則。
    pub errors: Vec<String>,
}

/// 変換の結果。
#[derive(Debug, Clone)]
pub struct Imported {
    pub file: LanguageFile,
    pub report: Report,
}

impl Imported {
    /// `language.toml` の文字列（先頭に報告をコメントで付ける）。
    pub fn to_toml(&self) -> String {
        let mut s = String::new();
        s.push_str("# zcode-langc import で .sublime-syntax から作った下書き。languages/README.md を見て直す。\n");
        s.push_str("# line_comment / run / check / indent は .sublime-syntax に無いので、手で書く。\n");
        if !self.report.errors.is_empty() {
            s.push_str("#\n# 変換できずに外した規則:\n");
            for e in &self.report.errors {
                s.push_str(&format!("#   {e}\n"));
            }
        }
        if !self.report.warnings.is_empty() {
            s.push_str("#\n# 近い形にした規則（意味が少し変わる）:\n");
            for w in &self.report.warnings {
                s.push_str(&format!("#   {w}\n"));
            }
        }
        s.push('\n');
        s.push_str(&toml::to_string(&self.file).unwrap_or_else(|e| format!("# TOML にできない: {e}\n")));
        s
    }
}

/// スコープ名 → トークンの種類（`text` なら色を付けない）。
pub fn scope_to_token(scope: &str) -> TokenKind {
    use TokenKind as T;
    // 空白区切りで複数書かれていたら、色が付く最初のもの
    for s in scope.split_whitespace() {
        let t = one_scope(s);
        if t != T::Text {
            return t;
        }
    }
    T::Text
}

fn one_scope(s: &str) -> TokenKind {
    use TokenKind as T;
    let has = |p: &str| s == p || s.starts_with(&format!("{p}."));
    if has("comment") || has("punctuation.definition.comment") {
        T::Comment
    } else if has("constant.character.escape") || has("constant.character.format") || has("constant.other.placeholder") {
        T::Escape
    } else if has("string") || has("punctuation.definition.string") {
        T::String
    } else if has("constant.numeric") {
        T::Number
    } else if has("constant.language") || has("constant") || has("variable.language") || has("support.constant") {
        T::Constant
    } else if has("keyword.control") || has("keyword.other.import") {
        T::Control
    } else if has("keyword.operator.word") || has("keyword.operator.logical") {
        T::Keyword
    } else if has("keyword.operator") {
        T::Operator
    } else if has("keyword") || has("storage.type") || has("storage.modifier") || has("storage") {
        T::Keyword
    } else if has("entity.name.function") || has("support.function") || has("variable.function") || has("meta.function-call.identifier") {
        T::Function
    } else if has("entity.name.class")
        || has("entity.name.type")
        || has("entity.name.struct")
        || has("entity.name.enum")
        || has("entity.name.trait")
        || has("support.type")
        || has("support.class")
        || has("entity.other.inherited-class")
    {
        T::Type
    } else if has("entity.name.tag") {
        T::Keyword
    } else if has("entity.other.attribute-name") || has("meta.annotation") || has("entity.name.decorator") || has("support.attribute") {
        T::Attribute
    } else if has("markup.heading") || has("entity.name.section") {
        T::Heading
    } else if has("meta.mapping.key") || has("entity.name.key") || has("support.type.property-name") {
        T::Key
    } else if has("markup.underline.link") || has("meta.link") {
        T::Link
    } else if has("variable.parameter") || has("variable.other.member") || has("variable") {
        T::Variable
    } else if has("invalid") {
        T::Invalid
    } else if has("punctuation") {
        T::Punctuation
    } else {
        T::Text
    }
}

fn sanitize(name: &str) -> String {
    name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect()
}

struct Ctx<'a> {
    vars: BTreeMap<String, String>,
    out: BTreeMap<String, Vec<Rule>>,
    defaults: BTreeMap<String, String>,
    report: Report,
    anon: usize,
    proto: bool,
    no_proto: Vec<String>,
    _p: std::marker::PhantomData<&'a ()>,
}

fn yaml_str(y: &Yaml) -> Option<String> {
    match y {
        Yaml::String(s) => Some(s.clone()),
        Yaml::Integer(i) => Some(i.to_string()),
        Yaml::Boolean(b) => Some(b.to_string()),
        Yaml::Real(r) => Some(r.clone()),
        _ => None,
    }
}

fn expand(pat: &str, vars: &BTreeMap<String, String>, depth: u32) -> Result<String, String> {
    if depth > 16 {
        return Err("variables の参照が深すぎる".into());
    }
    let mut out = String::new();
    let mut rest = pat;
    while let Some(i) = rest.find("{{") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        let Some(j) = after.find("}}") else {
            out.push_str(&rest[i..]);
            return Ok(out);
        };
        let name = after[..j].trim();
        match vars.get(name) {
            Some(v) => out.push_str(&expand(v, vars, depth + 1)?),
            None => return Err(format!("知らない変数 {{{{{name}}}}}")),
        }
        rest = &after[j + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

impl Ctx<'_> {
    /// 文脈 `name` の規則の列（YAML の配列）を変換する。
    fn context(&mut self, name: &str, items: &[Yaml]) {
        let mut rules = Vec::new();
        let mut include_proto = self.proto && name != "prototype";
        for (i, item) in items.iter().enumerate() {
            let loc = format!("{name}[{i}]");
            let h = match item.as_hash() {
                Some(h) => h,
                None => {
                    self.report.errors.push(format!("{loc}: 規則の形でない"));
                    continue;
                }
            };
            let get = |k: &str| h.get(&Yaml::String(k.into()));
            if let Some(v) = get("meta_include_prototype") {
                if v.as_bool() == Some(false) {
                    include_proto = false;
                    self.no_proto.push(name.to_string());
                }
            }
            for k in ["meta_scope", "meta_content_scope"] {
                if let Some(s) = get(k).and_then(yaml_str) {
                    let t = scope_to_token(&s);
                    if t != TokenKind::Text {
                        self.defaults.entry(name.to_string()).or_insert_with(|| t.name().to_string());
                    }
                }
            }
            if get("clear_scopes").is_some() {
                self.report.warnings.push(format!("{loc}: clear_scopes は無視した"));
            }
            if let Some(inc) = get("include").and_then(yaml_str) {
                if inc.starts_with("scope:") || inc.contains('/') || inc.contains('.') && !inc.contains("..") {
                    self.report.errors.push(format!("{loc}: ほかの構文の取り込み（include: {inc}）"));
                } else {
                    rules.push(Rule { include: Some(sanitize(&inc)), ..Rule::default() });
                }
                continue;
            }
            let Some(m) = get("match").and_then(yaml_str) else {
                if h.keys().any(|k| matches!(k.as_str(), Some("match" | "include"))) {
                    self.report.errors.push(format!("{loc}: match / include が読めない"));
                }
                continue;
            };
            if get("embed").is_some() || get("escape").is_some() || get("branch_point").is_some() || get("fail").is_some() {
                self.report.errors.push(format!("{loc}: embed / escape / branch_point / fail は変換しない（match = {m}）"));
                continue;
            }
            let expanded = match expand(&m, &self.vars, 0) {
                Ok(p) => p,
                Err(e) => {
                    self.report.errors.push(format!("{loc}: {e}（match = {m}）"));
                    continue;
                }
            };
            let conv = match convert(&expanded) {
                Ok(c) => c,
                Err(e) => {
                    self.report.errors.push(format!("{loc}: {e}（match = {m}）"));
                    continue;
                }
            };
            for w in &conv.warnings {
                self.report.warnings.push(format!("{loc}: {w}"));
            }
            let mut rule = Rule { match_: Some(conv.pattern.clone()), followed_by: conv.followed_by.clone(), ..Rule::default() };
            // 色
            let scope = get("scope").and_then(yaml_str);
            let mut token = scope.as_deref().map(scope_to_token).unwrap_or(TokenKind::Text);
            if let Some(caps) = get("captures").and_then(|c| c.as_hash()) {
                if token == TokenKind::Text {
                    let mut keys: Vec<(i64, String)> = caps
                        .iter()
                        .filter_map(|(k, v)| Some((k.as_i64().or_else(|| k.as_str()?.parse().ok())?, yaml_str(v)?)))
                        .collect();
                    keys.sort();
                    if let Some((_, s)) = keys.iter().find(|(_, s)| scope_to_token(s) != TokenKind::Text) {
                        token = scope_to_token(s);
                    }
                }
                if caps.len() > 1 || scope.is_some() {
                    self.report.warnings.push(format!("{loc}: captures は区間全体に 1 色（{}）", token.name()));
                }
            }
            if token != TokenKind::Text || scope.is_some() {
                rule.token = Some(token.name().to_string());
            }
            // 文脈の出入り
            let target = |this: &mut Self, y: &Yaml, what: &str| -> Option<OneOrMany> {
                match y {
                    Yaml::String(s) => Some(OneOrMany::One(sanitize(s))),
                    Yaml::Array(a) => {
                        // 名前の列か、名前の無い文脈（規則の列）か
                        if a.iter().all(|x| x.as_str().is_some()) {
                            Some(OneOrMany::Many(a.iter().map(|x| sanitize(x.as_str().unwrap())).collect()))
                        } else if a.iter().all(|x| x.as_hash().is_some()) {
                            Some(OneOrMany::One(this.anonymous(name, a)))
                        } else {
                            let mut v = Vec::new();
                            for x in a {
                                match x {
                                    Yaml::String(s) => v.push(sanitize(s)),
                                    Yaml::Array(inner) => v.push(this.anonymous(name, inner)),
                                    _ => {
                                        this.report.errors.push(format!("{loc}: {what} の形が読めない"));
                                        return None;
                                    }
                                }
                            }
                            Some(OneOrMany::Many(v))
                        }
                    }
                    _ => {
                        this.report.errors.push(format!("{loc}: {what} の形が読めない"));
                        None
                    }
                }
            };
            if let Some(p) = get("push").cloned() {
                rule.push = target(self, &p, "push");
            }
            if let Some(p) = get("set").cloned() {
                rule.set = target(self, &p, "set");
            }
            if let Some(p) = get("pop") {
                rule.pop = match p {
                    Yaml::Boolean(true) => Some(PopSpec::Flag(true)),
                    Yaml::Integer(n) if *n > 0 && *n < 256 => Some(PopSpec::Count(*n as u8)),
                    _ => None,
                };
            }
            if get("with_prototype").is_some() {
                self.report.warnings.push(format!("{loc}: with_prototype は無視した"));
            }
            let changes = rule.push.is_some() || rule.set.is_some() || rule.pop.is_some();
            if conv.pattern.is_empty() && !changes {
                self.report.errors.push(format!("{loc}: 空の match（先読みだけ）で文脈を変えない規則（match = {m}）"));
                continue;
            }
            // 正規表現を確かめる
            let mut bad = None;
            for p in std::iter::once(&conv.pattern).chain(conv.followed_by.iter()) {
                if let Err(e) = check_regex(p) {
                    bad = Some(format!("{e}（{p}）"));
                    break;
                }
            }
            if let Some(e) = bad {
                self.report.errors.push(format!("{loc}: DFA にできない: {e}（match = {m}）"));
                continue;
            }
            rules.push(rule);
        }
        if include_proto {
            rules.insert(0, Rule { include: Some("prototype".into()), ..Rule::default() });
        }
        self.out.insert(sanitize(name), rules);
    }

    fn anonymous(&mut self, parent: &str, items: &[Yaml]) -> String {
        self.anon += 1;
        let name = format!("{}__{}", sanitize(parent), self.anon);
        self.context(&name, items);
        name
    }
}

/// `.sublime-syntax` の文字列を変換する。
pub fn import_sublime_syntax(text: &str) -> Result<Imported, String> {
    let docs = YamlLoader::load_from_str(text).map_err(|e| format!("YAML が読めない: {e}"))?;
    let doc = docs.into_iter().next().ok_or("YAML が空")?;
    let get = |k: &str| &doc[k];
    let name = yaml_str(get("name")).unwrap_or_else(|| "imported".into());
    let mut vars = BTreeMap::new();
    if let Some(h) = get("variables").as_hash() {
        for (k, v) in h {
            if let (Some(k), Some(v)) = (yaml_str(k), yaml_str(v)) {
                vars.insert(k, v);
            }
        }
    }
    let contexts = get("contexts").as_hash().ok_or("contexts が無い")?;
    let proto = contexts.contains_key(&Yaml::String("prototype".into()));
    let mut cx = Ctx {
        vars,
        out: BTreeMap::new(),
        defaults: BTreeMap::new(),
        report: Report::default(),
        anon: 0,
        proto,
        no_proto: Vec::new(),
        _p: Default::default(),
    };
    for (k, v) in contexts {
        let (Some(k), Some(items)) = (yaml_str(k), v.as_vec()) else { continue };
        cx.context(&k, items);
    }
    // 取り込み先の無い include を外す
    let names: Vec<String> = cx.out.keys().cloned().collect();
    for (ctx, rules) in cx.out.iter_mut() {
        rules.retain(|r| match &r.include {
            Some(i) if !names.contains(i) => {
                cx.report.errors.push(format!("{ctx}: include の先が無い: {i}"));
                false
            }
            _ => true,
        });
    }
    let mut extensions: Vec<String> = Vec::new();
    for k in ["file_extensions", "hidden_file_extensions"] {
        if let Some(a) = get(k).as_vec() {
            extensions.extend(a.iter().filter_map(yaml_str));
        }
    }
    let mut first_line = None;
    if let Some(f) = yaml_str(get("first_line_match")) {
        match expand(&f, &cx.vars, 0).and_then(|p| convert(&p)) {
            Ok(c) if c.followed_by.is_none() && check_regex(&c.pattern).is_ok() => {
                first_line = Some(c.pattern);
                for w in c.warnings {
                    cx.report.warnings.push(format!("first_line_match: {w}"));
                }
            }
            _ => cx.report.errors.push(format!("first_line_match: 変換できない（{f}）")),
        }
    }
    let file = LanguageFile {
        name,
        extensions,
        first_line,
        contexts: cx.out,
        default_token: cx.defaults,
        ..LanguageFile::default()
    };
    // 全体として変換できるかを確かめる（push の先が無い、など）
    if let Err(e) = compile_language("imported", &file, "imported") {
        cx.report.errors.push(format!("全体の変換に失敗（手で直す）: {}", e.msg));
    }
    Ok(Imported { file, report: cx.report })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"%YAML 1.2
---
name: Toy
file_extensions: [toy, ty]
first_line_match: ^#!.*\btoy\b
scope: source.toy
variables:
  ident: '[A-Za-z_][A-Za-z0-9_]*'
contexts:
  prototype:
    - match: '#.*$'
      scope: comment.line.toy
  main:
    - match: \b(fn)\s+({{ident}})
      captures:
        1: storage.type.function.toy
        2: entity.name.function.toy
    - match: '{{ident}}(?=\()'
      scope: variable.function.toy
    - match: '"'
      scope: punctuation.definition.string.begin.toy
      push:
        - meta_scope: string.quoted.double.toy
        - match: \\.
          scope: constant.character.escape.toy
        - match: '"'
          pop: true
    - match: (?<=\.)\d+
      scope: constant.numeric.toy
    - match: (\w+)\s*=\s*\1
      scope: invalid.toy
    - match: \b(if|else)\b
      scope: keyword.control.toy
    - match: '<<(\w+)'
      embed: scope:text.heredoc
      escape: ^\1$
    - include: scope:source.other
"#;

    #[test]
    fn imports_toy_syntax() {
        let r = import_sublime_syntax(SAMPLE).unwrap();
        let f = &r.file;
        assert_eq!(f.name, "Toy");
        assert_eq!(f.extensions, vec!["toy", "ty"]);
        assert!(f.first_line.is_some());
        let main = &f.contexts["main"];
        // prototype が先頭に入る
        assert_eq!(main[0].include.as_deref(), Some("prototype"));
        // captures は 1 色（最初に色が付くもの = keyword）
        assert_eq!(main[1].token.as_deref(), Some("keyword"));
        // 末尾の先読み → followed_by
        assert_eq!(main[2].followed_by.as_deref(), Some(r"\("));
        assert_eq!(main[2].token.as_deref(), Some("function"));
        // 名前の無い文脈
        let anon = match &main[3].push {
            Some(OneOrMany::One(n)) => n.clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!(f.default_token.get(&anon).map(|s| s.as_str()), Some("string"));
        assert!(f.contexts.contains_key(&anon));
        // 後読みは外して警告、後方参照・embed・ほかの構文はエラー
        assert!(r.report.warnings.iter().any(|w| w.contains("後読み")), "{:?}", r.report.warnings);
        assert!(r.report.errors.iter().any(|e| e.contains("後方参照")), "{:?}", r.report.errors);
        assert!(r.report.errors.iter().any(|e| e.contains("embed")), "{:?}", r.report.errors);
        assert!(r.report.errors.iter().any(|e| e.contains("scope:source.other")), "{:?}", r.report.errors);
        // 出来上がりは変換できる
        let toml = r.to_toml();
        let back = zc_highlight::compile::parse_language_toml(&toml).unwrap();
        compile_language("toy", &back, "toy").unwrap();
        assert!(toml.contains("変換できずに外した規則"));
    }

    #[test]
    fn scope_mapping() {
        assert_eq!(scope_to_token("keyword.control.flow.python"), TokenKind::Control);
        assert_eq!(scope_to_token("storage.type.function.python"), TokenKind::Keyword);
        assert_eq!(scope_to_token("meta.function.python entity.name.function.python"), TokenKind::Function);
        assert_eq!(scope_to_token("punctuation.definition.string.begin.python"), TokenKind::String);
        assert_eq!(scope_to_token("constant.character.escape.python"), TokenKind::Escape);
        assert_eq!(scope_to_token("keyword.operator.logical.python"), TokenKind::Keyword);
        assert_eq!(scope_to_token("keyword.operator.arithmetic"), TokenKind::Operator);
        assert_eq!(scope_to_token("meta.something"), TokenKind::Text);
    }
}
