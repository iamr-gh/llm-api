# LLM API

Every agent application interacts with third-party LLMs over standard APIs. Services like litellm provide a basic compatibility layer for all these APIs. This repo is a minimal implementation of an API layer for the LLM services I care about, without the excess features present in other services. This repo is meant to be as small as possible and be updated rarely, to minimize surface area of security issues. This implementation should still be portable, and easily useable by most mainstream languages.

This repo will support the following LLM APIs:
- OpenRouter
- OpenAI
- Anthropic
- XAI
- Gemini

This repo will be maintained with AI, and may use other public sources as a reference. The purpose is not uniqueness, but targeted minimality.
