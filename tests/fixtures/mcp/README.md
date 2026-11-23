# MCP schemas

The official JSON Schemas of the two Model Context Protocol revisions
`kv-cli mcp` speaks (ADR 0006, decision 6), vendored so the MCP tests can
validate every message the server sends, offline:

| File | Revision | SHA-256 |
| --- | --- | --- |
| `schema-2026-07-28.json` | 2026-07-28 (current) | `907dcebce6aaeb16c21421ffef8c207093d88b9442fee340658a99cacb06b551` |
| `schema-2025-11-25.json` | 2025-11-25 (legacy, `initialize`) | `268a5f82ba70fd7e4b6dc4aa1e64f116f74b4d0edcb69dc046829c79dd4e97e7` |

Copied unchanged from `modelcontextprotocol/modelcontextprotocol` at commit
`862397cec702e8f828713071da501cf56158e8fc`, paths `schema/<revision>/schema.json`.
The specification is licensed under Apache-2.0 (contributions not yet
relicensed remain under MIT); see that repository's `LICENSE`.

Do not edit them. To update, replace them with copies from a newer commit and
update the commit and checksums above.
