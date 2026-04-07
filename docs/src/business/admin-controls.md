---
title: Admin Controls - Zed Business
description: Configure AI, collaboration, and data sharing settings for your entire Zed Business organization.
---

# Admin Controls

Owners and admins can configure settings that apply to every member of the organization.

These controls apply to Zed's server-side features, meaning anything that routes through Zed's infrastructure. They don't cover [bring-your-own-key (BYOK) configurations](../ai/llm-providers.md), [external agents](../ai/external-agents.md), or [third-party extensions](../extensions.md), since those work independently of Zed's servers.

## Accessing admin controls

Admin controls are available to owners and admins in the organization dashboard at
[dashboard.zed.dev](https://dashboard.zed.dev). Navigate to your organization, then
select **Data & Privacy** from the sidebar to configure these settings.

---

## Collaboration

Administrators can disable Zed's real-time collaboration features for the entire organization. This covers the features in the [Collaboration Panel](../collaboration/overview.md): [Channels](../collaboration/channels.md), shared projects, and voice chat.

<!-- TODO: confirm exact set of collaboration features covered by this toggle before launch -->
<!-- TODO: confirm behavior for members on older Zed clients when collaboration is disabled -->

When collaboration is disabled, members won't see collaboration features in their Zed client.

## Hosted AI models

The **Zed Model Provider** toggle controls whether members can use Zed's
[hosted AI models](../ai/models.md):

- **On:** Members can use Zed's hosted models for AI features.
- **Off:** Members must bring their own API keys via
  [Providers](../ai/llm-providers.md) or use
  [external agents](../ai/external-agents.md) for AI features.

## Edit Predictions

The **Edit Prediction** settings in Data & Privacy control
[Edit Predictions](../ai/edit-prediction.md) for the organization:

- **Edit Prediction:** Enable or disable Edit Predictions for all members.
- **Edit Prediction Feedback:** Allow or block members from submitting feedback
  on edit predictions. This setting is only available when Edit Prediction is
  enabled.

## Agent Thread Feedback

The **Agent Thread Feedback** toggle controls whether members can submit
feedback on agent thread responses. When disabled, members cannot rate or
provide feedback on AI agent conversations.

## Data sharing

By default, [data sharing with Zed for AI improvement](../ai/ai-improvement.md)
is opt-in for individual users not on a Business plan. Members choose
individually whether to share
[edit prediction training data](../ai/ai-improvement.md#edit-predictions) or
[AI feedback via ratings](../ai/ai-improvement.md#ai-feedback-with-ratings).

Administrators can enforce a no-sharing policy org-wide via the Agent Thread
Feedback and Edit Prediction Feedback toggles. Members cannot opt into either
form of data sharing when these are disabled.
