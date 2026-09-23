# Foundation Gateway (capture-shipping)

Chronicle stuurt tekst-captures naar de **Foundation Ingestie Gateway** op de
VPS: POST /api/ingest/capture met Bearer-auth.

## Endpoint

Standaard staan deze waarden in chronicle.toml (of via de defaults):

    [ship]
    enabled = true
    endpoint = "http://100.65.0.15:4577/api/ingest/capture"
    auth_token = "<token uit server/data/token.txt op de VPS>"
    interval_secs = 300

Het token staat op de VPS in /opt/Foundation/server/data/token.txt (hetzelfde
token als de Foundation UI gebruikt). Als alternatief kan de token in de
omgevingsvariabele CHRONICLE_INGEST_TOKEN (legacy: STASH_AUTH_TOKEN).

## Payload

Elke capture met tekst gaat als eigen JSON-object:

- content: index_text (gefilterd en geredigeerd)
- source: "chronicle-rs"
- title: "{app} — {venstertitel}"
- url: chronicle://capture/<capture-id>
- tags: [app]
- occurredAt: RFC3339-tijdstip van de capture

De Gateway bepaalt zelf de status ("observation") en dedupliceert op de url:
een capture die opnieuw wordt verstuurd wordt ge-updated, niet verdubbeld.
Captures zonder tekst (alleen beeld) worden overgeslagen.

## Gedrag

- De eerste keer start de shipper bij de nieuwste capture (geen historie).
- De cursor (SQLite ship_cursor) schuift pas na een geslaagde ronde; mislukte
  posts gaan de volgende ronde opnieuw.
- Alleen http:// over Tailscale; er is geen TLS-client ingebouwd.

Zie src/ship.rs voor de implementatie.
