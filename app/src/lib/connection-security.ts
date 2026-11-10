import type { ConnectionSecurity } from "../gen/ConnectionSecurity";
import type { ConnectionSecurityInfo } from "../gen/ConnectionSecurityInfo";
import type { ConnectionSecuritySpec } from "../gen/ConnectionSecuritySpec";

export function emptySecurity(): ConnectionSecurity {
  return { mode: null, ca_certificate: null, trust_server_certificate: false, certificate_hostname: null };
}

/** Token sign-in can retain a stricter saved policy, and never exposes a weaker effective policy. */
export function securityDraft(info: ConnectionSecurityInfo | null | undefined, facts: ConnectionSecuritySpec | null): ConnectionSecurity {
  const settings = { ...(info?.settings ?? emptySecurity()) };
  const mode = facts?.modes.find((mode) => mode.value === (settings.mode ?? info?.default_mode));
  if (info?.requires_verified_tls && settings.mode && !mode?.verifies_identity) settings.mode = info.effective_mode;
  if (info?.requires_verified_tls || (mode && !mode.allows_trust_certificate)) settings.trust_server_certificate = false;
  return settings;
}

export function securityMode(facts: ConnectionSecuritySpec, info: ConnectionSecurityInfo | null | undefined, settings: ConnectionSecurity) {
  const value = settings.mode ?? info?.default_mode ?? facts.default_mode;
  return facts.modes.find((mode) => mode.value === value);
}

export function sameSecurity(a: ConnectionSecurity, b: ConnectionSecurity): boolean {
  return a.mode === b.mode && a.ca_certificate === b.ca_certificate
    && a.trust_server_certificate === b.trust_server_certificate && a.certificate_hostname === b.certificate_hostname;
}
