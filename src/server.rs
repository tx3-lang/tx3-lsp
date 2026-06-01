use serde_json::Value;
use tower_lsp::{jsonrpc::Result, lsp_types::*, LanguageServer};
use tx3_lang::ast::Identifier;

use crate::{
    cmds, position_to_offset, span_contains, span_to_lsp_range,
    visitor::{find_symbol_in_program, SymbolAtOffset},
    Context,
};

#[tower_lsp::async_trait]
impl LanguageServer for Context {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                completion_provider: Some(Default::default()),
                definition_provider: Some(OneOf::Left(true)),
                type_definition_provider: Some(TypeDefinitionProviderCapability::Simple(true)),
                references_provider: Some(OneOf::Left(true)),
                document_symbol_provider: Some(OneOf::Left(true)),
                declaration_provider: Some(DeclarationCapability::Simple(true)),
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                semantic_tokens_provider: Some(
                    SemanticTokensServerCapabilities::SemanticTokensOptions(
                        SemanticTokensOptions {
                            work_done_progress_options: WorkDoneProgressOptions::default(),
                            legend: SemanticTokensLegend {
                                token_types: vec![
                                    SemanticTokenType::TYPE,
                                    SemanticTokenType::PARAMETER,
                                    SemanticTokenType::VARIABLE,
                                    SemanticTokenType::CLASS,
                                    SemanticTokenType::new("party"),
                                    SemanticTokenType::new("policy"),
                                    SemanticTokenType::FUNCTION,
                                    // SemanticTokenType::KEYWORD,
                                    // SemanticTokenType::PROPERTY,
                                ],
                                token_modifiers: vec![
                                    SemanticTokenModifier::DECLARATION,
                                    SemanticTokenModifier::DEFINITION,
                                    SemanticTokenModifier::READONLY,
                                    SemanticTokenModifier::STATIC,
                                ],
                            },
                            range: Some(true),
                            full: Some(SemanticTokensFullOptions::Bool(true)),
                        },
                    ),
                ),
                execute_command_provider: Some(ExecuteCommandOptions {
                    commands: vec!["generate-tir".to_string(), "generate-ast".to_string()],
                    work_done_progress_options: WorkDoneProgressOptions {
                        work_done_progress: None,
                    },
                }),
                ..Default::default()
            },
            server_info: Some(ServerInfo {
                name: "tx3-lsp".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "server initialized!")
            .await;
    }

    async fn completion(&self, _: CompletionParams) -> Result<Option<CompletionResponse>> {
        // Return empty completion list for now
        Ok(Some(CompletionResponse::Array(vec![])))
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        let uri = &params.text_document.uri;
        let document = self.documents.get(uri);

        if let Some(document) = document {
            let text = document.value().to_string();
            let rope = document.value();

            let ast = match tx3_lang::parsing::parse_string(text.as_str()) {
                Ok(ast) => ast,
                Err(_) => return Ok(None),
            };

            let tokens = self.collect_semantic_tokens(&ast, rope);

            Ok(Some(SemanticTokensResult::Tokens(SemanticTokens {
                result_id: None,
                data: tokens,
            })))
        } else {
            Ok(None)
        }
    }

    async fn semantic_tokens_range(
        &self,
        params: SemanticTokensRangeParams,
    ) -> Result<Option<SemanticTokensRangeResult>> {
        // TODO: optimize this for the specific range
        let full_params = SemanticTokensParams {
            text_document: params.text_document,
            work_done_progress_params: params.work_done_progress_params,
            partial_result_params: params.partial_result_params,
        };

        self.semantic_tokens_full(full_params).await.map(|result| {
            result.map(|tokens| match tokens {
                SemanticTokensResult::Tokens(t) => SemanticTokensRangeResult::Tokens(t),
                SemanticTokensResult::Partial(p) => SemanticTokensRangeResult::Partial(p),
            })
        })
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        let document = self.documents.get(uri);
        if let Some(document) = document {
            let text = document.value().to_string();

            let ast = match tx3_lang::parsing::parse_string(text.as_str()) {
                Ok(ast) => ast,
                Err(_) => return Ok(None),
            };

            let offset = position_to_offset(&text, position);

            if let Some(symbol) = find_symbol_in_program(&ast, offset) {
                let identifier = match symbol {
                    SymbolAtOffset::Identifier(x) => x,
                    SymbolAtOffset::TypeIdentifier(ty) => match ty {
                        tx3_lang::ast::Type::Custom(x) => x,
                        _ => return Ok(None),
                    },
                };

                for party in &ast.parties {
                    if party.name.value == identifier.value {
                        return Ok(Some(GotoDefinitionResponse::Scalar(Location {
                            uri: uri.clone(),
                            range: span_to_lsp_range(document.value(), &party.span),
                        })));
                    }
                }

                for policy in &ast.policies {
                    if policy.name.value == identifier.value {
                        return Ok(Some(GotoDefinitionResponse::Scalar(Location {
                            uri: uri.clone(),
                            range: span_to_lsp_range(document.value(), &policy.span),
                        })));
                    }
                }

                // A call-site callee (or the declaration name itself) jumps to
                // the function definition.
                for func in &ast.functions {
                    if func.name.value == identifier.value {
                        return Ok(Some(GotoDefinitionResponse::Scalar(Location {
                            uri: uri.clone(),
                            range: span_to_lsp_range(document.value(), &func.span),
                        })));
                    }
                }

                // Parameters and let-bindings referenced inside a function body.
                for func in &ast.functions {
                    if span_contains(&func.span, offset) {
                        for param in &func.parameters.parameters {
                            if param.name.value == identifier.value {
                                return Ok(Some(GotoDefinitionResponse::Scalar(Location {
                                    uri: uri.clone(),
                                    range: span_to_lsp_range(document.value(), &param.name.span),
                                })));
                            }
                        }

                        if let Some(body) = &func.body {
                            for binding in &body.let_bindings {
                                if binding.name.value == identifier.value {
                                    return Ok(Some(GotoDefinitionResponse::Scalar(Location {
                                        uri: uri.clone(),
                                        range: span_to_lsp_range(document.value(), &binding.span),
                                    })));
                                }
                            }
                        }
                    }
                }

                for tx in &ast.txs {
                    if span_contains(&tx.span, offset) {
                        for param in &tx.parameters.parameters {
                            if param.name.value == identifier.value {
                                return Ok(Some(GotoDefinitionResponse::Scalar(Location {
                                    uri: uri.clone(),
                                    range: span_to_lsp_range(document.value(), &tx.parameters.span),
                                })));
                            }
                        }

                        for input in &tx.inputs {
                            if input.name == identifier.value {
                                return Ok(Some(GotoDefinitionResponse::Scalar(Location {
                                    uri: uri.clone(),
                                    range: span_to_lsp_range(document.value(), &input.span),
                                })));
                            }
                        }

                        for output in &tx.outputs {
                            if let Some(output_name) = &output.name {
                                if output_name == identifier {
                                    return Ok(Some(GotoDefinitionResponse::Scalar(Location {
                                        uri: uri.clone(),
                                        range: span_to_lsp_range(document.value(), &output.span),
                                    })));
                                }
                            }
                        }

                        for reference in &tx.references {
                            if reference.name == identifier.value {
                                return Ok(Some(GotoDefinitionResponse::Scalar(Location {
                                    uri: uri.clone(),
                                    range: span_to_lsp_range(document.value(), &reference.span),
                                })));
                            }
                        }
                    }
                }
            }
        }

        Ok(None)
    }

    async fn references(&self, _: ReferenceParams) -> Result<Option<Vec<Location>>> {
        // Return empty references list for now
        Ok(Some(vec![]))
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        let document = self.documents.get(uri);
        if let Some(document) = document {
            let text = document.value().to_string();

            let ast = match tx3_lang::parsing::parse_string(text.as_str()) {
                Ok(ast) => ast,
                Err(_) => return Ok(None),
            };

            let offset = position_to_offset(&text, position);

            for party in &ast.parties {
                if span_contains(&party.span, offset) {
                    return Ok(Some(Hover {
                    contents: HoverContents::Markup(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: format!(
                            "**Party**: `{}`\n\nA party in the transaction. It can be an address for a script or a wallet.",
                            party.name.value
                        ),
                    }),
                    range: Some(span_to_lsp_range(document.value(), &party.span)),
                }));
                }
            }

            for policy in &ast.policies {
                if span_contains(&policy.span, offset) {
                    return Ok(Some(Hover {
                        contents: HoverContents::Markup(MarkupContent {
                            kind: MarkupKind::Markdown,
                            value: format!(
                                "**Policy**: `{}`\n\nA policy definition.",
                                policy.name.value
                            ),
                        }),
                        range: Some(span_to_lsp_range(document.value(), &policy.span)),
                    }));
                }
            }

            for type_def in &ast.types {
                if span_contains(&type_def.span, offset) {
                    return Ok(Some(Hover {
                        contents: HoverContents::Markup(MarkupContent {
                            kind: MarkupKind::Markdown,
                            value: format!(
                                "**Type**: `{}`\n\nA type definition.",
                                type_def.name.value
                            ),
                        }),
                        range: Some(span_to_lsp_range(document.value(), &type_def.span)),
                    }));
                }
            }

            for asset in &ast.assets {
                if span_contains(&asset.span, offset) {
                    return Ok(Some(Hover {
                        contents: HoverContents::Markup(MarkupContent {
                            kind: MarkupKind::Markdown,
                            value: format!(
                                "**Asset**: `{}`\n\nAn asset definition.",
                                asset.name.value
                            ),
                        }),
                        range: Some(span_to_lsp_range(document.value(), &asset.span)),
                    }));
                }
            }

            for func in &ast.functions {
                if span_contains(&func.span, offset) {
                    // Parameter-specific hover, when the cursor is on a parameter name.
                    if span_contains(&func.parameters.span, offset) {
                        for param in &func.parameters.parameters {
                            if span_contains(&param.name.span, offset) {
                                return Ok(Some(Hover {
                                    contents: HoverContents::Markup(MarkupContent {
                                        kind: MarkupKind::Markdown,
                                        value: format!(
                                            "**Parameter**: `{}`\n\n**Type**: `{}`",
                                            param.name.value, param.r#type
                                        ),
                                    }),
                                    range: Some(span_to_lsp_range(
                                        document.value(),
                                        &param.name.span,
                                    )),
                                }));
                            }
                        }
                    }

                    let params = func
                        .parameters
                        .parameters
                        .iter()
                        .map(|p| format!("{}: {}", p.name.value, p.r#type))
                        .collect::<Vec<_>>()
                        .join(", ");
                    let (label, blurb) = if func.builtin.is_some() {
                        ("Built-in function", "A function provided by the compiler.")
                    } else {
                        (
                            "Function",
                            "A user-defined function. Calls are inlined at compile time.",
                        )
                    };

                    return Ok(Some(Hover {
                        contents: HoverContents::Markup(MarkupContent {
                            kind: MarkupKind::Markdown,
                            value: format!(
                                "**{}**: `{}({}) -> {}`\n\n{}",
                                label, func.name.value, params, func.return_type, blurb
                            ),
                        }),
                        range: Some(span_to_lsp_range(document.value(), &func.span)),
                    }));
                }
            }

            for tx in &ast.txs {
                for input in &tx.inputs {
                    if span_contains(&input.span, offset) {
                        return Ok(Some(Hover {
                            contents: HoverContents::Markup(MarkupContent {
                                kind: MarkupKind::Markdown,
                                value: format!("**Input**: `{}`\n\nTransaction input.", input.name),
                            }),
                            range: Some(span_to_lsp_range(document.value(), &input.span)),
                        }));
                    }
                }

                for (i, output) in tx.outputs.iter().enumerate() {
                    if span_contains(&output.span, offset) {
                        let default_output = Identifier::new(format!("output {}", i + 1));
                        let name = output.name.as_ref().unwrap_or(&default_output);
                        return Ok(Some(Hover {
                            contents: HoverContents::Markup(MarkupContent {
                                kind: MarkupKind::Markdown,
                                value: format!(
                                    "**Output**: `{}`\n\nTransaction output.",
                                    name.value
                                ),
                            }),
                            range: Some(span_to_lsp_range(document.value(), &output.span)),
                        }));
                    }
                }

                if span_contains(&tx.parameters.span, offset) {
                    for param in &tx.parameters.parameters {
                        return Ok(Some(Hover {
                            contents: HoverContents::Markup(MarkupContent {
                                kind: MarkupKind::Markdown,
                                value: format!(
                                    "**Parameter**: `{}`\n\n**Type**: `{:?}`",
                                    param.name.value, param.r#type
                                ),
                            }),
                            range: Some(span_to_lsp_range(document.value(), &tx.parameters.span)),
                        }));
                    }
                }

                if span_contains(&tx.span, offset) {
                    let mut hover_text = format!("**Transaction**: `{}`\n\n", tx.name.value);

                    if !tx.parameters.parameters.is_empty() {
                        hover_text.push_str("**Parameters**:\n");
                        for param in &tx.parameters.parameters {
                            hover_text.push_str(&format!(
                                "- `{}`: `{:?}`\n",
                                param.name.value, param.r#type
                            ));
                        }
                        hover_text.push_str("\n");
                    }

                    if !tx.inputs.is_empty() {
                        hover_text.push_str("**Inputs**:\n");
                        for input in &tx.inputs {
                            hover_text.push_str(&format!("- `{}`\n", input.name));
                        }
                        hover_text.push_str("\n");
                    }

                    if !tx.outputs.is_empty() {
                        hover_text.push_str("**Outputs**:\n");
                        for (i, output) in tx.outputs.iter().enumerate() {
                            let default_output = Identifier::new(format!("output {}", i + 1));

                            let name = output.name.as_ref().unwrap_or(&default_output);
                            hover_text.push_str(&format!("- `{}`\n", name.value));
                        }
                    }

                    return Ok(Some(Hover {
                        contents: HoverContents::Markup(MarkupContent {
                            kind: MarkupKind::Markdown,
                            value: hover_text,
                        }),
                        range: Some(span_to_lsp_range(document.value(), &tx.span)),
                    }));
                }
            }
        }

        Ok(None)
    }

    // TODO: Add error handling and improve
    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        fn make_symbol(
            name: String,
            detail: String,
            kind: SymbolKind,
            range: Range,
            children: Option<Vec<DocumentSymbol>>,
        ) -> DocumentSymbol {
            #[allow(deprecated)]
            DocumentSymbol {
                name,
                detail: Some(detail),
                kind,
                range: range,
                selection_range: range,
                children: children,
                tags: Default::default(),
                deprecated: Default::default(),
            }
        }

        let mut symbols: Vec<DocumentSymbol> = Vec::new();
        let uri = &params.text_document.uri;
        let document = self.documents.get(uri);
        if let Some(document) = document {
            let text = document.value().to_string();
            let ast = tx3_lang::parsing::parse_string(text.as_str());
            if ast.is_ok() {
                let ast = ast.unwrap();
                for party in ast.parties {
                    symbols.push(make_symbol(
                        party.name.value.clone(),
                        "Party".to_string(),
                        SymbolKind::OBJECT,
                        span_to_lsp_range(document.value(), &party.span),
                        None,
                    ));
                }

                for policy in ast.policies {
                    symbols.push(make_symbol(
                        policy.name.value.clone(),
                        "Policy".to_string(),
                        SymbolKind::KEY,
                        span_to_lsp_range(document.value(), &policy.span),
                        None,
                    ));
                }

                for tx in ast.txs {
                    let mut children: Vec<DocumentSymbol> = Vec::new();
                    for parameter in tx.parameters.parameters {
                        children.push(make_symbol(
                            parameter.name.value.clone(),
                            format!("Parameter<{:?}>", parameter.r#type),
                            SymbolKind::FIELD,
                            span_to_lsp_range(document.value(), &tx.parameters.span),
                            None,
                        ));
                    }

                    for input in tx.inputs {
                        children.push(make_symbol(
                            input.name.clone(),
                            "Input".to_string(),
                            SymbolKind::OBJECT,
                            span_to_lsp_range(document.value(), &input.span),
                            None,
                        ));
                    }

                    for (i, output) in tx.outputs.iter().enumerate() {
                        let default_output = Identifier::new(format!("output {}", i + 1));

                        let name = output.name.as_ref().unwrap_or(&default_output);

                        children.push(make_symbol(
                            name.value.clone(),
                            "Output".to_string(),
                            SymbolKind::OBJECT,
                            span_to_lsp_range(document.value(), &output.span),
                            None,
                        ));
                    }

                    symbols.push(make_symbol(
                        tx.name.value.clone(),
                        "Tx".to_string(),
                        SymbolKind::METHOD,
                        span_to_lsp_range(document.value(), &tx.span),
                        Some(children),
                    ));
                }

                for func in ast.functions {
                    let mut children: Vec<DocumentSymbol> = Vec::new();
                    for parameter in &func.parameters.parameters {
                        children.push(make_symbol(
                            parameter.name.value.clone(),
                            format!("Parameter<{}>", parameter.r#type),
                            SymbolKind::FIELD,
                            span_to_lsp_range(document.value(), &func.parameters.span),
                            None,
                        ));
                    }

                    symbols.push(make_symbol(
                        func.name.value.clone(),
                        format!("Fn -> {}", func.return_type),
                        SymbolKind::FUNCTION,
                        span_to_lsp_range(document.value(), &func.span),
                        Some(children),
                    ));
                }
            }
        }
        Ok(Some(DocumentSymbolResponse::Nested(symbols)))
    }

    async fn symbol(&self, _: WorkspaceSymbolParams) -> Result<Option<Vec<SymbolInformation>>> {
        // Return empty workspace symbols list for now
        Ok(Some(vec![]))
    }

    async fn symbol_resolve(&self, params: WorkspaceSymbol) -> Result<WorkspaceSymbol> {
        dbg!(&params);
        Ok(params)
    }

    // TODO: not sure if using execute_command is a good idea, but it's the simplest way to return a value to the client without going outside of the lsp protocol
    async fn execute_command(&self, params: ExecuteCommandParams) -> Result<Option<Value>> {
        match cmds::handle_command(self, params).await {
            Ok(x) => Ok(x),
            Err(e) => {
                dbg!(&e);
                Err(e.into())
            }
        }
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        let version = params.text_document.version;
        let text = params.text_document.text.as_str();

        let diagnostics = self.process_document(uri.clone(), text).await;

        self.client
            .publish_diagnostics(uri, diagnostics, Some(version))
            .await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        let version = params.text_document.version;
        let text = params
            .content_changes
            .first()
            .map(|x| x.text.as_str())
            .unwrap_or("");

        let diagnostics = self.process_document(uri.clone(), text).await;

        self.client
            .publish_diagnostics(uri, diagnostics, Some(version))
            .await;
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.documents.remove(&params.text_document.uri);
    }
}
