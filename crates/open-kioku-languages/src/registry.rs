use crate::go::GoSemantics;
use crate::java::JavaSemantics;
use crate::python::PythonSemantics;
use crate::rust::RustSemantics;
use crate::semantics::LanguageSemantics;
use crate::typescript::{JavaScriptSemantics, TypeScriptSemantics};
use open_kioku_core::Language;

static JAVA_SEMANTICS: JavaSemantics = JavaSemantics;
static TYPESCRIPT_SEMANTICS: TypeScriptSemantics = TypeScriptSemantics;
static JAVASCRIPT_SEMANTICS: JavaScriptSemantics = JavaScriptSemantics;
static PYTHON_SEMANTICS: PythonSemantics = PythonSemantics;
static GO_SEMANTICS: GoSemantics = GoSemantics;
static RUST_SEMANTICS: RustSemantics = RustSemantics;

pub fn semantics_for(language: &Language) -> Option<&'static dyn LanguageSemantics> {
    match language {
        Language::Java => Some(&JAVA_SEMANTICS),
        Language::TypeScript => Some(&TYPESCRIPT_SEMANTICS),
        Language::JavaScript => Some(&JAVASCRIPT_SEMANTICS),
        Language::Python => Some(&PYTHON_SEMANTICS),
        Language::Go => Some(&GO_SEMANTICS),
        Language::Rust => Some(&RUST_SEMANTICS),
        _ => None,
    }
}
