## Changes
- Added PKCE-based `/auth` Google OAuth flow that auto-provisions the Gemini Code Assist companion project (loadCodeAssist + onboardCodeAssist), rejects ineligible accounts with structured 403 errors, and persists the credential once ready.
- Added `/v1beta/openai/models` to serve the embedded Gemini catalog in OpenAI-compatible format while keeping the native `/v1beta/models` response typed and logged.
- Router now separates auth vs Gemini routes and falls back to 404s for unknown paths; README refreshed with binary/docker quick start and OAuth walkthroughs.

## Upgrade notes
- No database migration required; restart to pick up the new `/auth` and `/v1beta/openai/models` routes.
