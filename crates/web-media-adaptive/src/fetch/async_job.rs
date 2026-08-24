//! Abortable async выполнение одного adaptive HTTP job-а.
//!
//! Модуль повторно использует policy и state владельца `AdaptiveHttpContext`,
//! но оставляет task lifecycle manifest executor-у: drop future должен сразу
//! уничтожать in-flight reqwest request superseded generation.

use source_core::{HttpBoundedFetchHop, HttpBoundedFetchRequest};
use web_media_transport_api::RedirectHopCount;

use super::{
    AdaptiveHttpContext, AdaptiveResourceSecretForwarding, AdaptiveTransportError, FetchJob,
    FetchSuccess, request_material,
};

/// Выполняет один adaptive job с manual redirect policy как abortable future.
pub(crate) async fn fetch_with_redirects_async(
    context: &AdaptiveHttpContext,
    job: FetchJob,
) -> Result<FetchSuccess, AdaptiveTransportError> {
    if context.cancellation.is_cancelled() {
        return Err(AdaptiveTransportError::Cancelled);
    }
    let mut target = job.target;
    let mut completed_hops = RedirectHopCount::none();
    let mut forward_secrets = matches!(
        job.secret_forwarding,
        AdaptiveResourceSecretForwarding::ForwardScoped
    );

    loop {
        let (request_target, headers) = request_material(
            &context.secrets,
            &target,
            job.purpose,
            job.query_application,
            forward_secrets,
        )?;
        let request = match job.byte_range {
            Some(byte_range) => HttpBoundedFetchRequest::range(
                request_target,
                headers,
                byte_range,
                job.purpose.fetch_kind(),
            ),
            None => HttpBoundedFetchRequest::full(
                request_target,
                headers,
                job.maximum_body_bytes,
                job.purpose.fetch_kind(),
            ),
        };
        let hop = match context
            .session
            .fetch_bounded_single_hop_abortable(request, &context.cancellation)
            .await
        {
            Ok(hop) => hop,
            Err(source_error) => {
                context.observe_endpoint_expiry(job.generation, job.purpose, &source_error);
                return Err(AdaptiveTransportError::Source(source_error));
            }
        };
        match hop {
            HttpBoundedFetchHop::Complete(response) => {
                let range_metadata = response.range_metadata().cloned();
                return Ok(FetchSuccess {
                    final_target: target,
                    bytes: response.into_bytes(),
                    range_metadata,
                });
            }
            HttpBoundedFetchHop::Redirect(redirect) => {
                let authorization = context.redirects.authorize_redirect(
                    &target,
                    redirect.target(),
                    completed_hops,
                )?;
                forward_secrets &= authorization.permits_secret_scope_check();
                target = redirect.target().clone();
                completed_hops = RedirectHopCount::new(completed_hops.value().saturating_add(1));
            }
        }
    }
}
