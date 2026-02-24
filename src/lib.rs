use std::str::FromStr as _;

use dashmap::DashMap;
use ropey::Rope;
use thiserror::Error;
use tower_lsp::jsonrpc::ErrorCode;
use tower_lsp::lsp_types::*;
use tower_lsp::Client;

mod ast_to_svg;
mod cmds;
mod server;
mod visitor;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Invalid command: {0}")]
    InvalidCommand(String),

    #[error("Invalid command args: {0}")]
    InvalidCommandArgs(String),

    #[error("Url Parse error: {0}")]
    ParseError(#[from] url::ParseError),

    #[error("Document not found: {0}")]
    DocumentNotFound(Url),

    #[error("Program parsing error: {0}")]
    ProgramParsingError(#[from] tx3_lang::parsing::Error),

    #[error("Tx3 Lowering error: {0}")]
    TxLoweringError(#[from] tx3_lang::lowering::Error),
}

impl From<&Error> for ErrorCode {
    fn from(err: &Error) -> Self {
        match err {
            Error::InvalidCommand(_) => ErrorCode::InvalidRequest,
            Error::ParseError(_) => ErrorCode::InvalidParams,
            Error::DocumentNotFound(_) => ErrorCode::InvalidParams,
            Error::InvalidCommandArgs(_) => ErrorCode::InvalidParams,
            Error::ProgramParsingError(_) => ErrorCode::InvalidRequest,
            Error::TxLoweringError(_) => ErrorCode::InvalidRequest,
        }
    }
}

impl From<Error> for tower_lsp::jsonrpc::Error {
    fn from(err: Error) -> Self {
        tower_lsp::jsonrpc::Error {
            code: From::from(&err),
            message: err.to_string().into(),
            data: None,
        }
    }
}

pub fn char_index_to_line_col(rope: &Rope, idx: usize) -> (usize, usize) {
    let line = rope.char_to_line(idx);
    let line_start = rope.line_to_char(line);
    let col = idx - line_start;
    (line, col)
}

pub fn position_to_offset(text: &str, position: Position) -> usize {
    let mut offset = 0;
    for (line_num, line) in text.lines().enumerate() {
        if line_num == position.line as usize {
            offset += position.character.min(line.len() as u32) as usize;
            break;
        }
        offset += line.len() + 1;
    }
    offset
}

pub fn span_contains(span: &tx3_lang::ast::Span, offset: usize) -> bool {
    offset >= span.start && offset < span.end
}

pub fn span_to_lsp_range(rope: &Rope, loc: &tx3_lang::ast::Span) -> Range {
    let (start_line, start_col) = char_index_to_line_col(rope, loc.start);
    let (end_line, end_col) = char_index_to_line_col(rope, loc.end);
    let start = Position::new(start_line as u32, start_col as u32);
    let end = Position::new(end_line as u32, end_col as u32);
    Range::new(start, end)
}

fn parse_error_to_diagnostic(rope: &Rope, err: &tx3_lang::parsing::Error) -> Diagnostic {
    let range = span_to_lsp_range(rope, &err.span);
    let message = err.message.clone();
    let source = err.src.clone();

    Diagnostic {
        range,
        severity: Some(DiagnosticSeverity::ERROR),
        source: Some(source),
        message,
        ..Default::default()
    }
}

fn analyze_error_to_diagnostic(rope: &Rope, err: &tx3_lang::analyzing::Error) -> Diagnostic {
    let range = span_to_lsp_range(rope, err.span());
    let message = err.to_string();
    let source = err.src().unwrap_or("tx3").to_string();

    Diagnostic {
        range,
        severity: Some(DiagnosticSeverity::ERROR),
        source: Some(source),
        message,
        ..Default::default()
    }
}

fn analyze_report_to_diagnostic(
    rope: &Rope,
    report: &tx3_lang::analyzing::AnalyzeReport,
) -> Vec<Diagnostic> {
    report
        .errors
        .iter()
        .map(|err| analyze_error_to_diagnostic(rope, err))
        .collect()
}

#[derive(Debug)]
pub struct Context {
    pub client: Client,
    pub documents: DashMap<Url, Rope>,
    //asts: DashMap<Url, tx3_lang::ast::Program>,
}

impl Context {
    fn is_type_field_reference(
        ast: &tx3_lang::ast::Program,
        identifier: &str,
        offset: usize,
    ) -> bool {
        for type_def in &ast.types {
            if crate::span_contains(&type_def.span, offset) {
                for case in &type_def.cases {
                    for field in &case.fields {
                        if identifier == field.r#type.to_string() {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }
    fn collect_semantic_tokens(
        &self,
        ast: &tx3_lang::ast::Program,
        rope: &Rope,
    ) -> Vec<SemanticToken> {
        const TOKEN_TYPE: u32 = 0;
        const TOKEN_PARAMETER: u32 = 1;
        const TOKEN_VARIABLE: u32 = 2;
        const TOKEN_CLASS: u32 = 3;
        const TOKEN_PARTY: u32 = 4;
        const TOKEN_POLICY: u32 = 5;
        const TOKEN_FUNCTION: u32 = 6;
        // const TOKEN_KEYWORD: u32 = 7;
        // const TOKEN_PROPERTY: u32 = 8;

        const MOD_DECLARATION: u32 = 1 << 0;
        const MOD_DEFINITION: u32 = 1 << 1;

        #[derive(Debug, Clone)]
        struct TokenInfo {
            range: Range,
            token_type: u32,
            token_modifiers: u32,
        }

        let mut token_infos: Vec<TokenInfo> = Vec::new();
        let text = rope.to_string();

        let mut processed_spans = std::collections::HashSet::new();

        for offset in 0..text.len() {
            if let Some(symbol) = crate::visitor::find_symbol_in_program(ast, offset) {
                match symbol {
                    crate::visitor::SymbolAtOffset::Identifier(identifier) => {
                        // Skip if we've already processed this exact span
                        let span_key = (identifier.span.start, identifier.span.end);
                        if processed_spans.contains(&span_key) {
                            continue;
                        }
                        processed_spans.insert(span_key);

                        let token_type = if ast
                            .parties
                            .iter()
                            .any(|p| p.name.value == identifier.value)
                        {
                            TOKEN_PARTY
                        } else if ast
                            .policies
                            .iter()
                            .any(|p| p.name.value == identifier.value)
                        {
                            TOKEN_POLICY
                        } else if ast.types.iter().any(|t| t.name.value == identifier.value) {
                            TOKEN_TYPE
                        } else if Context::is_type_field_reference(ast, &identifier.value, offset) {
                            TOKEN_TYPE
                        } else if ast.assets.iter().any(|a| a.name.value == identifier.value) {
                            TOKEN_CLASS
                        } else {
                            let mut found_type = None;

                            for tx in &ast.txs {
                                if tx.name.value == identifier.value {
                                    found_type = Some(TOKEN_FUNCTION);
                                    break;
                                }

                                if crate::span_contains(&tx.span, offset) {
                                    for param in &tx.parameters.parameters {
                                        if param.name.value == identifier.value {
                                            found_type = Some(TOKEN_PARAMETER);
                                            break;
                                        }
                                    }
                                }

                                if found_type.is_some() {
                                    break;
                                }
                            }
                            found_type.unwrap_or(TOKEN_VARIABLE)
                        };

                        token_infos.push(TokenInfo {
                            range: crate::span_to_lsp_range(rope, &identifier.span),
                            token_type,
                            token_modifiers: MOD_DECLARATION | MOD_DEFINITION,
                        });
                    }
                    visitor::SymbolAtOffset::TypeIdentifier(_x) => {
                        // TODO: wait for the introduction of `TypeAnnotation` in AST

                        // token_infos.push(TokenInfo {
                        //     range: crate::span_to_lsp_range(rope, &x.span),
                        //     token_type: TOKEN_TYPE,
                        //     token_modifiers: MOD_DECLARATION | MOD_DEFINITION,
                        // });
                    }
                }
            }
        }
        token_infos.sort_by(|a, b| match a.range.start.line.cmp(&b.range.start.line) {
            std::cmp::Ordering::Equal => a.range.start.character.cmp(&b.range.start.character),
            other => other,
        });

        token_infos.dedup_by(|a, b| a.range.start == b.range.start && a.range.end == b.range.end);

        let mut semantic_tokens = Vec::new();
        let mut prev_line = 0;
        let mut prev_start = 0;

        for token_info in token_infos {
            let line = token_info.range.start.line;
            let start = token_info.range.start.character;
            let length = token_info.range.end.character.saturating_sub(start);

            if length == 0 {
                continue;
            }

            let delta_line = line.saturating_sub(prev_line);
            let delta_start = if delta_line == 0 {
                start.saturating_sub(prev_start)
            } else {
                start
            };

            semantic_tokens.push(SemanticToken {
                delta_line,
                delta_start,
                length,
                token_type: token_info.token_type,
                token_modifiers_bitset: token_info.token_modifiers,
            });

            prev_line = line;
            prev_start = start;
        }

        semantic_tokens
    }

    pub fn new_for_client(client: Client) -> Self {
        Self {
            client,
            documents: DashMap::new(),
        }
    }

    fn get_document(&self, url_arg: &str) -> Result<Rope, Error> {
        let uri = Url::from_str(url_arg)?;

        let document = self
            .documents
            .get(&uri)
            .ok_or(Error::DocumentNotFound(uri))?;

        Ok(document.value().clone())
    }

    fn get_document_program(&self, url_arg: &str) -> Result<tx3_lang::ast::Program, Error> {
        let document = self.get_document(url_arg)?;
        tx3_lang::parsing::parse_string(document.to_string().as_str()).map_err(Error::ProgramParsingError)
    }

    async fn process_document(&self, uri: Url, text: &str) -> Vec<Diagnostic> {
        let rope = Rope::from_str(text);
        self.documents.insert(uri.clone(), rope.clone());

        let ast = tx3_lang::parsing::parse_string(text);

        match ast {
            Ok(mut ast) => {
                let analysis = tx3_lang::analyzing::analyze(&mut ast);
                analyze_report_to_diagnostic(&rope, &analysis)
            }
            Err(e) => vec![parse_error_to_diagnostic(&rope, &e)],
        }
    }
}
