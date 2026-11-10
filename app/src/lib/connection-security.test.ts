import { expect, test } from "bun:test";
import { emptySecurity, securityDraft, securityMode } from "./connection-security";
import { connectionSecurityFacts, ENGINES } from "./engines";
import { demoSecurityInfo, validateDemoSettings } from "./demo-security";

test("Every server provider resolves defaults, imported aliases and cloud-auth restrictions from generated facts", () => {
  for (const engine of ENGINES) {
    const facts = connectionSecurityFacts(engine);
    const info = demoSecurityInfo(engine, "db.example.com", "password");
    if (!facts) { expect(info).toBeUndefined(); continue; }
    expect(info?.effective_mode).toBe(facts.default_mode);
    expect(demoSecurityInfo(engine, "localhost", "password")?.default_mode).toBe(facts.local_default_mode);
    const insecure = facts.modes.find((mode) => mode.allows_plaintext);
    expect(insecure).toBeDefined();
    const imported = demoSecurityInfo(engine, "db.example.com", "password", { [facts.mode_option]: insecure!.value.toLowerCase() });
    expect(imported?.settings.mode).toBe(insecure!.value);
    const cloud = demoSecurityInfo(engine, "db.example.com", "entra", { [facts.mode_option]: insecure!.value });
    const draft = securityDraft(cloud, facts);
    expect(securityMode(facts, cloud, draft)?.verifies_identity).toBe(true);
    expect(() => validateDemoSettings(engine, cloud, { environment: "production", security: imported!.settings })).toThrow();
  }
});

test("A saved unknown TLS option remains visible until the user chooses a supported setting", () => {
  const facts = connectionSecurityFacts("postgres")!;
  const info = demoSecurityInfo("postgres", "db.example.com", "password", { sslmode: "unrecognized" })!;
  const draft = securityDraft(info, facts);
  expect(draft.mode).toBe("unrecognized");
  expect(securityMode(facts, info, draft)).toBeUndefined();
  expect(() => validateDemoSettings("postgres", info, { environment: "production", security: draft })).toThrow();
  expect(securityMode(facts, info, emptySecurity())?.value).toBe("verify-full");
});

test("Imported SQL Server trust flags retain their native boolean meaning", () => {
  for (const value of ["YES", "on", "mandatory", "strict"]) {
    const info = demoSecurityInfo("mssql", "db.example.com", "password", { trustServerCertificate: value });
    expect(info?.settings.trust_server_certificate).toBe(true);
  }
});

test("Strict encryption drafts clear an incompatible trust override without changing saved settings", () => {
  const facts = connectionSecurityFacts("mssql")!;
  const info = demoSecurityInfo("mssql", "db.example.com", "password", { encrypt: "strict", trustServerCertificate: "true" })!;
  expect(securityDraft(info, facts).trust_server_certificate).toBe(false);
  expect(info.settings.trust_server_certificate).toBe(true);
});
