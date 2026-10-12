use serde_json::{Value, json};

#[derive(Debug)]
pub struct ServiceError {
    pub code: String,
    pub message: String,
    pub status_code: u16,
    pub details: serde_json::Map<String, Value>,
}

impl ServiceError {
    pub fn new(code: &str, message: &str, status_code: u16) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            status_code,
            details: serde_json::Map::new(),
        }
    }

    pub fn io(error: std::io::Error) -> Self {
        let code = match error.kind() {
            std::io::ErrorKind::NotFound => "ENOENT",
            std::io::ErrorKind::PermissionDenied => "EACCES",
            std::io::ErrorKind::AlreadyExists => "EEXIST",
            std::io::ErrorKind::NotADirectory => "ENOTDIR",
            _ => "TOOL_IO_FAILED",
        };
        Self::new(code, &error.to_string(), 500)
    }

    pub fn value(&self) -> Value {
        let mut value =
            json!({"code":self.code,"message":self.message,"statusCode":self.status_code});
        for (key, detail) in &self.details {
            value[key] = detail.clone();
        }
        value
    }

    pub fn with_details(mut self, details: Value) -> Self {
        if let Some(details) = details.as_object() {
            self.details.extend(details.clone());
        }
        self
    }
}

pub type ServiceResult<T> = Result<T, ServiceError>;

pub fn envelope(result: ServiceResult<Value>) -> Value {
    match result {
        Ok(value) => json!({"value":value}),
        Err(error) => json!({"error":error.value()}),
    }
}
