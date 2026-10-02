//! A diagnostic: a code, a message, a primary span, and optional labels, notes, help and fixes.

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
    pub primary: Label,
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
            primary: Label { span, message: None },
            secondary: Vec::new(),
            notes: Vec::new(),
            help: Vec::new(),
            fixes: Vec::new(),
        }
    }

    /// A message under the primary span.
    pub fn with_label(mut self, message: impl Into<String>) -> Diagnostic {
        self.primary.message = Some(message.into());
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

    pub fn span(&self) -> Span {
        self.primary.span
    }

    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

/// Sorts diagnostics into a stable order: by file, then position, then code. Duplicates (same
/// code and primary span) are dropped.
pub fn sort_and_dedup(diags: &mut Vec<Diagnostic>) {
    diags.sort_by(|a, b| {
        (a.primary.span.file, a.primary.span.start, a.primary.span.end, a.code.as_str()).cmp(&(
            b.primary.span.file,
            b.primary.span.start,
            b.primary.span.end,
            b.code.as_str(),
        ))
    });
    diags.dedup_by(|a, b| a.code == b.code && a.primary.span == b.primary.span);
}
