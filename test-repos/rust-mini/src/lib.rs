pub struct ImportWorker;

impl ImportWorker {
    pub fn import_failed_api() -> bool {
        retry_import()
    }
}

pub fn retry_import() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_retry_import() {
        assert!(retry_import());
    }
}

