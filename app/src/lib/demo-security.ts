import type { AuthKind } from "../gen/AuthKind";
import type { Engine } from "../gen/Engine";
import type { ConnectionSecurityInfo } from "../gen/ConnectionSecurityInfo";
import type { ConnectionSettings } from "../gen/ConnectionSettings";
import { connectionSecurityFacts } from "./engines";
import { emptySecurity } from "./connection-security";
import { t } from "./i18n";

const normalize = (value: string) => value.trim().toLowerCase().replace(/[ _-]/g, "");

/** Demo metadata uses the provider's generated facts. The desktop resolves this from its saved spec in Rust. */
export function demoSecurityInfo(engine: Engine, host: string, auth: AuthKind, options: Record<string, string> = {}): ConnectionSecurityInfo | undefined {
  const facts = connectionSecurityFacts(engine);
  if (!facts) return undefined;
  const read = (...names: (string | null)[]) => names.flatMap((name) => name ? Object.entries(options).filter(([key]) => normalize(key) === normalize(name)).map(([, value]) => value) : [])[0] ?? null;
  const raw = read(...facts.mode_aliases);
  const mode = facts.modes.find((mode) => [mode.value, ...mode.aliases].some((value) => raw !== null && normalize(value) === normalize(raw)));
  const requires_verified_tls = auth === "entra" || auth === "aws-iam";
  const local = host === "localhost" || host.endsWith(".localhost") || /^127\./.test(host) || host === "::1";
  const default_mode = !requires_verified_tls && local ? facts.local_default_mode : facts.default_mode;
  return {
    settings: { mode: mode?.value ?? raw, ca_certificate: read(...facts.ca_aliases), trust_server_certificate: ["true", "yes", "1", "on", "mandatory", "strict"].includes((read(facts.trust_certificate_option) ?? "").toLowerCase()), certificate_hostname: read(facts.hostname_option) },
    default_mode, effective_mode: requires_verified_tls && mode && !mode.verifies_identity ? facts.default_mode : mode?.value ?? raw ?? default_mode, requires_verified_tls,
  };
}

export function validateDemoSettings(engine: Engine, info: ConnectionSecurityInfo | undefined, settings: ConnectionSettings): void {
  const facts = connectionSecurityFacts(engine);
  if (!facts) {
    const empty = emptySecurity();
    if (Object.keys(empty).some((key) => settings.security[key as keyof typeof empty] !== empty[key as keyof typeof empty])) throw { code: "invalid_input", message: t("core.connection_security.not_applicable"), hint: null };
    return;
  }
  const mode = facts.modes.find((mode) => mode.value === (settings.security.mode ?? info?.default_mode ?? facts.default_mode));
  if (!mode) throw { code: "invalid_input", message: t("core.connection_security.unknown_mode", { mode: settings.security.mode ?? "" }), hint: null };
  if (info?.requires_verified_tls && (!mode.verifies_identity || settings.security.trust_server_certificate)) throw { code: "invalid_input", message: t("core.connection_security.verified_required"), hint: null };
  if (settings.security.trust_server_certificate && !mode.allows_trust_certificate) throw { code: "invalid_input", message: t("core.connection_security.unsupported_certificate_options"), hint: null };
}
