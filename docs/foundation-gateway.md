# Foundation Gateway (capture-shipping)

Capture stuurt tekst-captures naar de **Foundation Ingestie Gateway** op de
VPS: POST /api/ingest/capture met Bearer-auth. Foundation is het geheugen met
één API — repo: [Bojanni050/Foundation](https://github.com/Bojanni050/Foundation).
Dit is de enige uitgaande pijp; de oude Stash/selection-pass-route naar
Hindsight is vervangen en uit de repo verwijderd.

## Endpoint

Standaard staan deze waarden in capture.toml (of via de defaults):

    [ship]
    enabled = true
    endpoint = "http://100.65.0.15:4577/api/ingest/capture"
    auth_token = "<token uit server/data/token.txt op de VPS>"
    interval_secs = 300

Het token staat op de VPS in /opt/Foundation/server/data/token.txt (hetzelfde
token als de Foundation UI gebruikt). Als alternatief kan de token in de
omgevingsvariabele CAPTURE_INGEST_TOKEN (legacy: CHRONICLE_INGEST_TOKEN,
STASH_AUTH_TOKEN).

## Payload

Elke capture met tekst gaat als eigen JSON-object:

- content: index_text (gefilterd en geredigeerd)
- source: "capture-rs"
- title: "{app} — {venstertitel}"
- url: capture://capture/<capture-id>
- tags: [app]
- occurredAt: RFC3339-tijdstip van de capture

Het contract is **exact** (`server/ingestPolicy.js` aan de Foundation-kant):

- verplicht voor `capture`: `content` én `source`
- toegestaan: `title`, `url`, `tags`, `attachments`, `occurredAt`
- verboden voor iedereen: `status`, `providerConversationId`, `contentHash`,
  `id`, `objectType`, `ingestedAt`, `updatedAt` — die zijn server-eigendom;
  een client die er toch een meestuurt krijgt 422, niet een stille drop
- `turns` en `sourceProvider` horen bij de chat-entry-point, niet bij capture
- **onbekende velden = 422**: een tikfout in een veldnaam moet hard falen

De Gateway bepaalt zelf de status ("observation") en leidt de
dedup-identiteit af uit `url` (`provider_conversation_id`): een capture die
opnieuw wordt verstuurd wordt ge-updated, niet verdubbeld. Captures zonder
tekst (alleen beeld) worden overgeslagen.

## Diagnose

Laatste 50 binnenkomende objecten (zelfde token):

    curl -H "Authorization: Bearer $TOKEN" \
      http://100.65.0.15:4577/api/ingest/recent

## Gedrag

- De eerste keer start de shipper bij de nieuwste capture (geen historie).
- De cursor (SQLite ship_cursor) schuift pas na een geslaagde ronde; mislukte
  posts gaan de volgende ronde opnieuw.
- Alleen http:// over Tailscale; er is geen TLS-client ingebouwd. Wil je over
  het publieke internet, zet dan een reverse proxy met TLS vóór Foundation
  (zie DEPLOYMENT.md in die repo) en verander `ship.endpoint`.
- Foundation bindt standaard alleen op loopback; externe callers komen
  binnen als `CHRONICLE_HOST` op een Tailscale-interface is gezet.

Zie src/ship.rs voor de implementatie.
