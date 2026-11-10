// Process-scoped Pi extension. Verified: Pi v1.5.1 core/extensions/types.ts.
export function requestFailureReason(message) {
  const text = String(message).toLowerCase();
  if (/free usage exceeded/.test(text)) return "The model's free usage quota has been exceeded.";
  if (/quota|credit|billing|balance|\b402\b/.test(text)) return "The model provider reports insufficient quota or credits.";
  if (/rate.?limit|\b429\b/.test(text)) return "The model provider is rate limiting this request.";
  if (/unauthorized|authentication|api.?key|\b401\b|\b403\b/.test(text)) return "Model provider authentication failed. Check the Agent's credentials.";
  if (/overload|\b503\b/.test(text)) return "The model provider is temporarily unavailable.";
  if (/timeout|timed out|connection|network|fetch failed/.test(text)) return "The model request encountered a network error.";
  return "The model request failed. Check the Agent's model and provider configuration.";
}
export default function requestFeedback(pi) {
  pi.on("agent_end", (event, context) => {
    const last = event.messages?.at(-1);
    if (last?.role !== "assistant" || last.stopReason !== "error" || !last.errorMessage) return;
    // Existing adapter forwards attempts/delay; notify adds the missing safe
    // cause through its existing extension_ui_request/notify ACP conversion.
    context.ui.notify(requestFailureReason(last.errorMessage), "warning");
  });
}
