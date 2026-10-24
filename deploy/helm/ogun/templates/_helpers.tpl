{{- define "ogun.name" -}}
{{- .Chart.Name | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "ogun.fullname" -}}
{{- if contains .Chart.Name .Release.Name -}}
{{- .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name .Chart.Name | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}

{{- define "ogun.labels" -}}
app.kubernetes.io/name: {{ include "ogun.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version }}
{{- end -}}

{{- define "ogun.selectorLabels" -}}
app.kubernetes.io/name: {{ include "ogun.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{- define "ogun.image" -}}
{{ .Values.image.repository }}:{{ .Values.image.tag | default .Chart.AppVersion }}
{{- end -}}

{{- define "ogun.oidcSecret" -}}
{{- if .Values.oidc.existingSecret -}}
{{ .Values.oidc.existingSecret }}
{{- else -}}
{{ include "ogun.fullname" . }}-oidc
{{- end -}}
{{- end -}}

{{/* host[:port] of publicUrl */}}
{{- define "ogun.host" -}}
{{- $u := urlParse .Values.publicUrl -}}
{{- $u.host -}}
{{- end -}}

{{/* server.json */}}
{{- define "ogun.serverJson" -}}
{{- $providers := list -}}
{{- range .Values.oidc.providers -}}
{{- $p := dict "id" .id "display_name" .displayName "issuer" .issuer "client_id" .clientId "scopes" (.scopes | default (list "openid" "email" "profile")) -}}
{{- if .clientSecretKey -}}
{{- $_ := set $p "client_secret_file" (printf "/etc/ogun/oidc/%s" .clientSecretKey) -}}
{{- end -}}
{{- $providers = append $providers $p -}}
{{- end -}}
{{- $cfg := dict
  "public_url" .Values.publicUrl
  "listen" "0.0.0.0:8080"
  "behind_proxy" true
  "trusted_proxies" .Values.server.trustedProxies
  "state_dir" "/var/lib/ogun/ogun-server"
  "studio_dir" "/usr/share/ogun/studio-web"
  "daemons" (dict "default" (dict "ogun_home" "/var/lib/ogun" "profile" "server" "credential" "server"))
  "oidc" (dict "providers" $providers)
  "sessions" (dict "idle_timeout_minutes" (int .Values.server.sessions.idleTimeoutMinutes) "absolute_timeout_hours" (int .Values.server.sessions.absoluteTimeoutHours))
  "webauthn" (dict "attestation" .Values.server.webauthn.attestation)
  "remote_approvals" .Values.server.remoteApprovals
  "mcp" (dict "enabled" .Values.server.mcp.enabled)
  "sync" (dict "enabled" .Values.server.sync.enabled "daemon" "default")
-}}
{{- $cfg = mergeOverwrite $cfg .Values.server.extraConfig -}}
{{- toPrettyJson $cfg -}}
{{- end -}}
