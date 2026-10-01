use crate::{Agent, CLEANED, Error, Request, Response};
use runwell_node::ProcessState;

impl Agent {
    /// Compact tombstones make delayed lifecycle calls safe without retaining
    /// job payloads. Conflicting attempts cannot resurrect a cleaned job.
    pub(crate) async fn cleaned_response(
        &self,
        request: &Request,
    ) -> Result<Option<Response>, Error> {
        let Some(key) = request.key() else {
            return Ok(None);
        };
        if key.job_id <= 0 || key.attempt <= 0 {
            return Err(Error::Protocol);
        }
        let Some(record) = self.store.lease_record(key.job_id).await? else {
            return Ok(None);
        };
        if record.attempt != key.attempt {
            return Err(Error::Protocol);
        }
        if record.phase != CLEANED {
            return Ok(None);
        }
        Ok(Some(match request {
            Request::Inspect(_) => Response::Process(ProcessState::Absent),
            Request::Measure(_) => {
                Response::Measurement(record.measurement()?.ok_or(Error::Protocol)?)
            }
            Request::Admit { .. } => Response::Admitted(false),
            _ => Response::Ok,
        }))
    }
}
