//! A blocking host service borrows nested execution from its live runtime call.
//! The channel cannot outlive that call or register another root invocation.
use super::*;
use crate::{HostPluginInvocation, HostPluginInvoker};

type NestedResult = Result<Value, HostCapabilityError>;

struct NestedPublication {
    request: HostPluginInvocation,
    reply: oneshot::Sender<NestedResult>,
}

struct ChannelHostPluginInvoker {
    publications: UnboundedSender<NestedPublication>,
}

impl HostPluginInvoker for ChannelHostPluginInvoker {
    fn invoke_plugin(&self, request: HostPluginInvocation) -> NestedResult {
        let (reply, result) = oneshot::channel();
        self.publications
            .send(NestedPublication { request, reply })
            .map_err(|_| closed_nested_call())?;
        result.blocking_recv().map_err(|_| closed_nested_call())?
    }
}

struct HostDelegation<'runtime> {
    plugin_id: String,
    invocation_id: String,
    call_id: String,
    capability_id: String,
    access: InvocationAccess<'runtime>,
    deadline: Instant,
}

pub(super) async fn invoke_scoped(
    request: HostCapabilityRequest,
    access: InvocationAccess<'_>,
    deadline: Instant,
) -> NestedResult {
    let delegation = HostDelegation {
        plugin_id: request.plugin_id.clone(),
        invocation_id: request.invocation_id.clone(),
        call_id: request.call_id.clone(),
        capability_id: request.capability_id.clone(),
        access,
        deadline,
    };
    let (publications, mut requests) = unbounded_channel();
    let (reply, mut result) = oneshot::channel();
    delegation.spawn_broker(request, publications, reply);
    delegation.wait_broker(&mut requests, &mut result).await
}

impl HostDelegation<'_> {
    fn spawn_broker(
        &self,
        request: HostCapabilityRequest,
        publications: UnboundedSender<NestedPublication>,
        reply: oneshot::Sender<NestedResult>,
    ) {
        let broker = Arc::clone(&self.access.broker);
        let context = self.access.context.clone();
        thread::spawn(move || {
            let nested = ChannelHostPluginInvoker { publications };
            let _ = reply.send(broker.invoke_with_nested(request, &context, &nested));
        });
    }

    async fn wait_broker(
        &self,
        requests: &mut UnboundedReceiver<NestedPublication>,
        result: &mut oneshot::Receiver<NestedResult>,
    ) -> NestedResult {
        let cancellation = self.access.context.cancellation();
        let mut sequence = 0_u64;
        loop {
            tokio::select! {
                _ = cancellation.cancelled() => return Err(host_call_cancelled(&self.capability_id)),
                _ = tokio::time::sleep(self.deadline.saturating_duration_since(Instant::now())) =>
                    return Err(host_call_timed_out(&self.capability_id)),
                response = &mut *result => return response.unwrap_or_else(|_| Err(host_call_executor_failed(&self.capability_id))),
                Some(call) = requests.recv() => {
                    sequence = sequence.checked_add(1).ok_or_else(closed_nested_call)?;
                    let output = self.invoke_nested(call.request, sequence).await;
                    let _ = call.reply.send(output);
                }
            }
        }
    }

    async fn invoke_nested(&self, request: HostPluginInvocation, sequence: u64) -> NestedResult {
        let input = Value::Object(serde_json::Map::from_iter([
            ("plugin_id".into(), Value::String(request.plugin_id)),
            ("export_id".into(), Value::String(request.export_id)),
            ("input".into(), request.input),
        ]));
        let authorization = self.nested_authorization(input.clone(), sequence)?;
        invoke_host_capability_async(
            Arc::clone(&self.access.broker),
            authorization,
            self.access.context,
            self.deadline,
        )
        .await?;
        let output = Box::pin(self.access.plugin_system.invoke_plugin_async(
            &self.plugin_id,
            input,
            self.access.context,
        ))
        .await?;
        output.get("output").cloned().ok_or_else(closed_nested_call)
    }

    fn nested_authorization(
        &self,
        input: Value,
        sequence: u64,
    ) -> Result<HostCapabilityRequest, HostCapabilityError> {
        let required_version = self
            .access
            .declared_host_capabilities
            .get("plugin.invoke")
            .ok_or_else(|| undeclared_host_capability("plugin.invoke"))?;
        Ok(HostCapabilityRequest {
            plugin_id: self.plugin_id.clone(),
            invocation_id: self.invocation_id.clone(),
            call_id: format!("{}:nested:{sequence}", self.call_id),
            capability_id: "plugin.invoke".into(),
            required_version: required_version.clone(),
            input,
        })
    }
}

fn closed_nested_call() -> HostCapabilityError {
    HostCapabilityError::new(
        "plugin.invoke",
        "host_nested_invocation_closed",
        "nested publication channel closed; expected an active parent host call",
        false,
    )
}
