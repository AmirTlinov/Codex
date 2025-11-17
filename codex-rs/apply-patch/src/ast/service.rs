use crate::ApplyPatchError;
use crate::ast::parse_tree_for_language;
use crate::ast::query::AstQueryMatch;
use crate::ast::query::run_query;
use crate::ast::resolve_locator;
use crate::ast::semantic::SemanticModel;
use crate::ast_ops::AstOperationSpec;
use crate::ast_transform::plan_ast_operation;
use crate::parser::ParseError::InvalidPatchError;
use std::path::Path;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::mpsc::Sender;
use std::thread;

use crate::ast_transform::AstEditPlan;

use once_cell::sync::Lazy;

pub struct AstServiceHandle {
    tx: Sender<ServiceRequest>,
}

pub fn global_service_handle() -> &'static AstServiceHandle {
    static HANDLE: Lazy<AstServiceHandle> = Lazy::new(AstServiceHandle::spawn);
    &HANDLE
}

impl AstServiceHandle {
    fn spawn() -> Self {
        let (tx, rx) = mpsc::channel();
        thread::Builder::new()
            .name("ast-service".into())
            .spawn(move || {
                let worker = AstServiceWorker {};
                while let Ok(request) = rx.recv() {
                    worker.handle_request(request);
                }
            })
            .unwrap_or_else(|err| panic!("failed to spawn ast-service thread: {err}"));
        Self { tx }
    }

    pub fn plan_operation(
        &self,
        path: &Path,
        source: String,
        spec: AstOperationSpec,
    ) -> Result<AstEditPlan, ApplyPatchError> {
        let (responder, receiver) = mpsc::channel();
        self.tx
            .send(ServiceRequest::PlanOperation {
                path: path.to_path_buf(),
                source,
                spec,
                responder,
            })
            .map_err(|err| service_unavailable(err.to_string()))?;
        receiver
            .recv()
            .map_err(|err| service_unavailable(err.to_string()))?
    }

    pub fn run_query(
        &self,
        path: &Path,
        source: String,
        language: Option<String>,
        query: String,
    ) -> Result<Vec<AstQueryMatch>, ApplyPatchError> {
        let (responder, receiver) = mpsc::channel();
        self.tx
            .send(ServiceRequest::RunQuery {
                path: path.to_path_buf(),
                source,
                language,
                query,
                responder,
            })
            .map_err(|err| service_unavailable(err.to_string()))?;
        receiver
            .recv()
            .map_err(|err| service_unavailable(err.to_string()))?
    }
}

struct AstServiceWorker;

type OperationResponder = mpsc::Sender<Result<AstEditPlan, ApplyPatchError>>;
type QueryResponder = mpsc::Sender<Result<Vec<AstQueryMatch>, ApplyPatchError>>;

enum ServiceRequest {
    PlanOperation {
        path: PathBuf,
        source: String,
        spec: AstOperationSpec,
        responder: OperationResponder,
    },
    RunQuery {
        path: PathBuf,
        source: String,
        language: Option<String>,
        query: String,
        responder: QueryResponder,
    },
}

impl AstServiceWorker {
    fn handle_request(&self, request: ServiceRequest) {
        match request {
            ServiceRequest::PlanOperation {
                path,
                source,
                spec,
                responder,
            } => {
                let result = plan_ast_operation(&path, &source, &spec);
                let _ = responder.send(result);
            }
            ServiceRequest::RunQuery {
                path,
                source,
                language,
                query,
                responder,
            } => {
                let result = self.handle_query(&path, &source, language, &query);
                let _ = responder.send(result);
            }
        }
    }

    fn handle_query(
        &self,
        path: &Path,
        source: &str,
        language_override: Option<String>,
        query: &str,
    ) -> Result<Vec<AstQueryMatch>, ApplyPatchError> {
        let language = match language_override {
            Some(lang) => lang,
            None => {
                let locator = resolve_locator(path).ok_or_else(|| {
                    ApplyPatchError::ParseError(InvalidPatchError(format!(
                        "Cannot infer language for {}",
                        path.display()
                    )))
                })?;
                locator.language().to_string()
            }
        };
        let tree = parse_tree_for_language(&language, source).map_err(|err| {
            ApplyPatchError::ParseError(InvalidPatchError(format!(
                "Failed to parse {} for queries: {err}",
                path.display()
            )))
        })?;
        let semantic = SemanticModel::build(&tree, source);
        run_query(&language, &tree, source, query, &semantic)
    }
}

fn service_unavailable(message: String) -> ApplyPatchError {
    ApplyPatchError::ParseError(InvalidPatchError(format!(
        "AST service unavailable: {message}"
    )))
}
