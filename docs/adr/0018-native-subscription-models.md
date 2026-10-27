# 0018: Native subscription inference for compiled workflow model operators

Accepted, 28 October 2026, implementing the owner's 0.0.1c subscription requirement.

The operator sandbox cannot access another app's signed-in CLI. Compiled built-in model operators can instead run as trusted native code, like core operators, when every enabled provider in the prepared ladder is a reserved local-agent endpoint. Node provider selection, model access, network access and reserved budget still apply. Select a subscription-only ladder (or narrow the node's providers) to use this path. Mixed API/subscription ladders retain the child sandbox.

The host never executes the manifest's command on this path. Installed/replaced manifests always retain the child sandbox. The native library runs the same bounded implementation as ogun-op-llm and the existing protocol collector validates declared output ports, scratch containment, metadata and usage. Deadlines/cancellation drop the inference future and terminate its CLI process tree. No API secret is supplied to this native path.

Installed CLIs authenticate themselves. Their own file, shell, fetch and MCP tools remain disabled/refused by the transport. They receive the selected artifact text, not access to the workflow's workspace or artifact directory. Model replies remain data, interpreted by each compiled operator. No custom code, external command or untrusted manifest gains host execution.
