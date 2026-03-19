---
title: Admin Controls - Zed Business
description: Configure AI, collaboration, and data sharing settings for your entire Zed Business organization.
---

# Admin Controls

Owners and admins can configure settings that apply to every member of the organization.

These controls apply to Zed's server-side features, meaning anything that routes through Zed's infrastructure. They don't cover bring-your-own-key (BYOK) configurations or third-party extensions, since those work independently of Zed's servers.

## Accessing Admin Controls

<!-- TODO: document exact location in dashboard before launch -->

Admin controls are available to owners and admins from the organization dashboard at [dashboard.zed.dev](https://dashboard.zed.dev).

---

## Collaboration

Administrators can disable Zed's real-time collaboration features for the entire organization. This covers the features in the [Collaboration Panel](../collaboration/overview.md): [Channels](../collaboration/channels.md), shared projects, and voice chat.

<!-- TODO: confirm exact set of collaboration features covered by this toggle before launch -->
<!-- TODO: confirm behavior for members on older Zed clients when collaboration is disabled -->

When collaboration is disabled, members won't see collaboration features in their Zed client.

## Hosted AI Models

Administrators can control which of Zed's [hosted AI models](../ai/models.md) are available to members:

- Disable all Zed-hosted models entirely, so members must use their own API keys via [Providers](../ai/llm-providers.md) if they want AI features
- Enable or disable access by model provider (Anthropic, OpenAI, Google, etc.)

This applies to Zed's hosted model service only. Members who bring their own API keys are not affected.

<!-- TODO: confirm exact model provider controls available at launch -->

## Edit Predictions

Administrators can disable [Edit Predictions](../ai/edit-prediction.md) for all members of the organization.

## Data Sharing

By default, [data sharing with Zed for AI improvement](../ai/ai-improvement.md) is opt-in. Members choose individually whether to share [edit prediction training data](../ai/ai-improvement.md#edit-predictions) or [AI feedback via ratings](../ai/ai-improvement.md#ai-feedback-with-ratings).

Administrators can enforce a no-sharing policy org-wide, blocking members from opting into either form of data sharing. This is enforced server-side, so members can't opt back in individually.

<!-- TODO: confirm exact scope of data sharing controls before launch -->
