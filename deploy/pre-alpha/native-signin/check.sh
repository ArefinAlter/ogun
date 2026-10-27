#!/usr/bin/env bash
# Checks the hub offers desktop sign-in and still serves the browser sign-in. No credentials are used.
set -euo pipefail
HUB="${1:?usage: check.sh https://hub.example}"
PROVIDERS="$(curl -fsS "$HUB/auth/providers")"
echo "providers: $PROVIDERS"
ISSUER="$(printf '%s' "$PROVIDERS" | python3 -c '
import json, sys
d = json.load(sys.stdin)
assert d["providers"], "no browser provider"
assert d.get("native"), "the hub does not offer desktop sign-in yet"
n = d["native"][0]
print(n["issuer"])
')"
echo "native issuer: $ISSUER"
curl -fsS "$ISSUER/.well-known/openid-configuration" | python3 -c '
import json, sys
d = json.load(sys.stdin)
assert "S256" in d.get("code_challenge_methods_supported", ["S256"]), "no PKCE"
print("issuer ok:", d["issuer"])
'
# A public client with a loopback redirect must reach the provider's login page (no secret needed).
CODE="$(curl -s -o /dev/null -w '%{http_code}' -G "$ISSUER/auth" \
  --data-urlencode "client_id=ogun-studio" --data-urlencode "response_type=code" \
  --data-urlencode "redirect_uri=http://127.0.0.1:43111/callback" \
  --data-urlencode "scope=openid email profile offline_access" \
  --data-urlencode "state=check" \
  --data-urlencode "code_challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM" \
  --data-urlencode "code_challenge_method=S256")"
echo "authorization endpoint answered $CODE (302 or 200 is good; 400 means the client or the redirect is refused)"
[ "$CODE" = "302" ] || [ "$CODE" = "200" ]
