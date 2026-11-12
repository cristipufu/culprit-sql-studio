use super::*;

pub(super) struct BlobRequest {
    endpoint: Arc<Endpoint>,
    path: String,
    authorization: RequestAuthorization,
}
impl BlobRequest {
    pub fn new(endpoint: Arc<Endpoint>, path: String, authorization: RequestAuthorization) -> Self {
        Self { endpoint, path, authorization }
    }
    pub fn upload(self, body: Upload, condition: Condition) -> Result<ObjectWrite> {
        let date = storage_date();
        let auth = self.authorization.header("PUT", &self.path, body.len(), Some(&condition), &date);
        let mut request = agent(self.endpoint.origin.starts_with("https://"))
            .put(format!("{}{}", self.endpoint.origin, self.path))
            .header("x-ms-version", STORAGE_VERSION)
            .header("x-ms-blob-type", "BlockBlob")
            .header("Content-Length", body.len().to_string())
            .header("Content-Type", "application/octet-stream")
            .header("x-ms-date", &date)
            .header("Authorization", auth.as_str());
        request = match &condition {
            Condition::Absent => request.header("If-None-Match", "*"),
            Condition::Matches(etag) => request.header("If-Match", etag),
        };
        let response = body.send(request)?;
        write_result(&response, &condition)
    }
    pub fn download(self) -> Result<Option<(ureq::http::Response<ureq::Body>, String, u64)>> {
        let date = storage_date();
        let auth = self.authorization.header("GET", &self.path, 0, None, &date);
        let response = agent(self.endpoint.origin.starts_with("https://"))
            .get(format!("{}{}", self.endpoint.origin, self.path))
            .header("x-ms-version", STORAGE_VERSION)
            .header("x-ms-date", &date)
            .header("Authorization", auth.as_str())
            .header("Accept-Encoding", "identity")
            .call()
            .map_err(|_| transport_error())?;
        if response.status().as_u16() == 404 {
            return Ok(None);
        }
        if response.status().as_u16() != 200 {
            return Err(transport_error());
        }
        let (etag, bytes) = response_metadata(&response)?;
        Ok(Some((response, etag, bytes)))
    }
}
impl Upload {
    fn send(
        self,
        request: ureq::RequestBuilder<ureq::typestate::WithBody>,
    ) -> Result<ureq::http::Response<ureq::Body>> {
        let result = match self {
            Self::Bytes(bytes) => request.send(bytes),
            Self::File { file, bytes } => {
                request.send(ureq::SendBody::from_owned_reader(std::io::Read::take(file, bytes)))
            }
        };
        result.map_err(|_| transport_error())
    }
}
fn write_result(response: &ureq::http::Response<ureq::Body>, condition: &Condition) -> Result<ObjectWrite> {
    let code = response.headers().get("x-ms-error-code");
    match response.status().as_u16() {
        201 => Ok(ObjectWrite::Written),
        412 if code.is_some_and(|code| code == "ConditionNotMet") => Ok(ObjectWrite::ConditionNotMet),
        409 if matches!(condition, Condition::Absent) && code.is_some_and(|code| code == "BlobAlreadyExists") => {
            Ok(ObjectWrite::ConditionNotMet)
        }
        _ => Err(transport_error()),
    }
}
