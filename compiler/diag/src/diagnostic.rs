//! A diagnostic: a code, a message, a primary span, and optional labels, notes, help and fixes.
//! An internal compiler error ([`Diagnostic::internal`]) has no span: it's about the compiler,
//! not a place in the program.

use crate::codes::Code;
use crate::source::Span;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
        }
    }
}

/// A span with an optional message, shown under the source line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Label {
    pub span: Span,
    pub message: Option<String>,
}

/// One text replacement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub span: Span,
    pub replacement: String,
}

/// A suggested fix: a description and the edits that make it. Applying every edit of one fix
/// must leave the program free of this diagnostic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fix {
    pub message: String,
    pub edits: Vec<Edit>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: Code,
    pub severity: Severity,
    pub message: String,
    /// Where the problem is; `None` only for an internal compiler error.
    pub primary: Option<Label>,
    pub secondary: Vec<Label>,
    pub notes: Vec<String>,
    pub help: Vec<String>,
    pub fixes: Vec<Fix>,
}

impl Diagnostic {
    /// An error with `code`'s severity, at `span`.
    pub fn new(code: Code, span: Span, message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            code,
            severity: code.severity(),
            message: message.into(),
            primary: Some(Label { span, message: None }),
            secondary: Vec::new(),
            notes: Vec::new(),
            help: Vec::new(),
            fixes: Vec::new(),
        }
    }

    /// A bug in the compiler, not a problem with the program (I0001): no span, and a note
    /// saying so.
    pub fn internal(message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            code: crate::codes::I0001,
            severity: Severity::Error,
            message: message.into(),
            primary: None,
            secondary: Vec::new(),
            notes: vec![
                "this is a bug in the wrela compiler, not in your program; please report it".into(),
            ],
            help: Vec::new(),
            fixes: Vec::new(),
        }
    }

    /// A message under the primary span.
    pub fn with_label(mut self, message: impl Into<String>) -> Diagnostic {
        if let Some(p) = &mut self.primary {
            p.message = Some(message.into());
        }
        self
    }

    pub fn with_secondary(mut self, span: Span, message: impl Into<String>) -> Diagnostic {
        self.secondary.push(Label { span, message: Some(message.into()) });
        self
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Diagnostic {
        self.notes.push(note.into());
        self
    }

    pub fn with_help(mut self, help: impl Into<String>) -> Diagnostic {
        self.help.push(help.into());
        self
    }

    /// A fix that replaces `span` with `replacement`.
    pub fn with_fix(
        self,
        message: impl Into<String>,
        span: Span,
        replacement: impl Into<String>,
    ) -> Diagnostic {
        self.with_fix_edits(message, vec![Edit { span, replacement: replacement.into() }])
    }

    pub fn with_fix_edits(mut self, message: impl Into<String>, edits: Vec<Edit>) -> Diagnostic {
        self.fixes.push(Fix { message: message.into(), edits });
        self
    }

    /// The primary span: `None` for an internal compiler error.
    pub fn span(&self) -> Option<Span> {
        self.primary.as_ref().map(|p| p.span)
    }

    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

/// Sorts diagnostics into a stable order: by file, then position, then code. Duplicates (same
/// code and primary span) are dropped.
pub fn sort_and_dedup(diags: &mut Vec<Diagnostic>) {
    // Internal errors first: they explain whatever else went wrong.
    let key = |d: &Diagnostic| {
        let s = d.span();
        (s.is_some(), s.map(|s| (s.file, s.start, s.end)), d.code.as_str())
    };
    diags.sort_by(|a, b| key(a).cmp(&key(b)));
    diags.dedup_by(|a, b| {
        a.code == b.code && a.span() == b.span() && (a.span().is_some() || a.message == b.message)
    });
}
