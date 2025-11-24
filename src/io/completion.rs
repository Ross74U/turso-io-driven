use turso_core::Completion as TursoCompletion;

pub enum WrappedCompletion {
    TursoCompletion(TursoCompletion),
    Completion(Completion),
}

pub enum CompletionType {
    Accept,
    ReadSocket,
    WriteSocket,
}

pub struct Completion {
    completion_type: CompletionType,
}

impl Completion {
    // TODO: callback handling, typed constructor
}
