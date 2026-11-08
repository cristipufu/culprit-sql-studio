# German (de) glossary

Read this before changing `locales/de.json`. Every message says a thing the same way; when a term below doesn't fit a
sentence, rephrase the sentence rather than the term.

## Voice and form

- **Address the reader as „Sie“**, never „du“. Imperatives use the Sie form: „Fügen Sie einen gruppierten Index hinzu.“
  Buttons and menu items are infinitives instead („Speichern“, „Verbindung testen“), never imperatives with „Sie“.
- **Concise, technical, plain.** Write what a German DBA would write in a ticket, not a word-for-word copy of the
  English. A button stays one or two words; a column header one word where possible.
- **Capitalization:** German noun capitalization everywhere; otherwise sentence case for headings, labels, buttons and
  column headers („Gruppierter Index fehlt“, „Pläne vergleichen“, „Letzte Ausführung“). Adjectives in a label stay
  lowercase unless they start it.
- **Lowercase fragments stay lowercase** where English writes them lowercase (labels after a dot: „· 3 abgebrochen“),
  except nouns.

## Typography

- Quotation marks: „…“ (U+201E, U+201C). Nested: ‚…‘.
- Ellipsis: keep „…“ wherever English has it („Wird gespeichert…“).
- No space before `: ; ! ?`. A dash in running text is „ – “ (en dash with spaces).
- Compound nouns are written as one word where German does (Wartezeit, Indexsuche, Sperrwartezeit, Tabellenscan,
  Ausführungsplan) and with a hyphen when one part is a name, an abbreviation, SQL or code (CPU-Zeit, I/O-Latenz,
  SQL-Server-Instanz, `tempdb`-Überlauf, HOT-Updates, JIT-Kompilierung, HTML-Seite).
- Numbers, sizes, durations and percentages come formatted (decimal comma, thousands dot): never write digits into a
  message yourself except where English does („200 Zeilen“, „30 Sekunden“).
- Keyboard keys as English shows them (Enter, Esc, ⌘K).

## Kept in English

Engine and product names (Culprit, SQL Server, PostgreSQL, MySQL, MariaDB, SQLite, YugabyteDB, Azure SQL, Entra ID,
AWS, Azure, VS Code, GitHub), SQL keywords (SELECT, UPDATE, VACUUM, ANALYZE, AFTER/BEFORE trigger), setting names
(`max_connections`, `track_io_timing`, fillfactor), wait type names (PAGEIOLATCH_SH, LCK_M_X), catalog and view names
(pg_stats, sys.dm_exec_query_stats, performance_schema), plan operator names as the engine prints them (Index Seek,
Key Lookup, Hash Match, Seq Scan), and these words DBAs use in English in German: Workload, Deadlock, Heap, Snapshot,
Baseline, Dashboard, Live, Cache, Latch, Flush, Query Store, JIT, HOT, Tablet, Slot, Tab, Pull Request, Markdown.

## Terms

| English | German | Notes |
|---|---|---|
| finding | Befund (pl. Befunde) | a Culprit result; „keine Befunde“ |
| check | Prüfung | a Culprit check; „Prüfungsabdeckung“ for check coverage |
| scan (Culprit's) | Scan (der, pl. Scans); scannen, erneut scannen | „Letzter Scan fehlgeschlagen“ |
| re-scan | erneut scannen | |
| report | Bericht | |
| workload | Workload (die) | |
| query | Abfrage | |
| statement | Anweisung | „SQL-Anweisung“ when ambiguous |
| table-valued parameter, bulk insert, array | tabellenwertiger Parameter, Masseneinfügung, Array | SQL syntax stays unchanged |
| plan | Plan / Ausführungsplan | „Plan“ in labels, „Ausführungsplan“ when introducing it |
| estimated / actual plan | geschätzter / tatsächlicher Plan | |
| operator | Operator | |
| optimizer | Optimierer | Microsoft Learn: Abfrageoptimierer |
| run (n., one execution) | Ausführung | „pro Ausführung“; „Letzte Ausführung“ |
| run (v.) | ausführen | button: „Ausführen“ |
| execution count / calls | Ausführungen / Aufrufe | |
| compile / compilation | kompilieren / Kompilierung | |
| thread scheduling | Einplanung von Threads | Zuteilung bereiter Threads zur CPU |
| wait (n.) | Wartezeit (time), Wartevorgang (one wait) | headings: „Wartezeiten“ |
| wait type | Wartetyp | |
| wait event (PostgreSQL) | Warteereignis | |
| lock | Sperre | lock wait: Sperrwartezeit; lock escalation: Sperrausweitung |
| table lock / row lock / page lock | Tabellensperre / Zeilensperre / Seitensperre | |
| latch | Latch | page latch: Seitenlatch |
| blocking / blocked / blocker | Blockierung / blockiert / blockierende Sitzung | |
| deadlock | Deadlock (der) | |
| index | Index (pl. Indizes) | |
| clustered / nonclustered index | gruppierter / nicht gruppierter Index | Microsoft Learn |
| primary key / foreign key | Primärschlüssel / Fremdschlüssel | |
| key column / included column | Schlüsselspalte / eingeschlossene Spalte | |
| seek | Suche (Indexsuche) | operator name stays „Index Seek“ |
| scan (index, table) | Scan (Indexscan, Tabellenscan) | operator name stays „Index Scan“, „Seq Scan“ |
| lookup | Lookup (Schlüssel-Lookup) | |
| heap | Heap | |
| forwarded record / forwarding pointer | weitergeleiteter Datensatz / Weiterleitungszeiger | |
| table / view / column / schema | Tabelle / Sicht / Spalte / Schema | |
| row | Zeile | |
| page | Seite | |
| read / write (n.) | Lesevorgang / Schreibvorgang | short labels: „Lesen“, „Schreiben“; „Lesevorgänge“ in counts |
| reads from disk | Lesevorgänge vom Datenträger | disk: Datenträger |
| flush | Flush (pl. Flushes) | log flush: Log-Flush |
| log (transaction log / WAL) | Log (Transaktionslog), WAL | |
| latency | Latenz | |
| throughput | Durchsatz | |
| duration / elapsed time | Dauer / verstrichene Zeit | |
| regression | Regression | „ist langsamer geworden“ in sentences |
| opportunity | Verbesserungsmöglichkeit | short: „Möglichkeiten“ only where space forces it |
| severity / impact | Schweregrad / Auswirkung | high/medium/low impact: hohe/mittlere/geringe Auswirkung |
| evidence | Belege (pl.) | „Unzureichende Belege“ |
| fix (n.) / fix (v.) | Behebung / beheben | heading „Behebung“ |
| verify | prüfen, ob … behoben ist; heading „So prüfen Sie es“ | button „Behebung prüfen“ |
| baseline | Baseline | |
| snapshot | Snapshot | |
| connection | Verbindung | |
| session | Sitzung | end a session: Sitzung beenden |
| transaction | Transaktion | roll back: zurücksetzen (zurückgesetzt); commit: committen (committet); as nouns in compact labels: Rollback, Commit |
| idle in transaction | im Leerlauf in einer Transaktion | |
| server / database / instance | Server / Datenbank / Instanz | |
| engine | Engine (die) / Datenbank-Engine | |
| node / cluster | Knoten / Cluster | |
| replica / replication / slot | Replikat / Replikation / Slot | |
| setting | Einstellung | |
| statistics | Statistiken | column statistics: Spaltenstatistiken; histogram: Histogramm |
| counters | Zähler | „seit dem letzten Zurücksetzen der Zähler“ |
| reset (counters) | zurücksetzen | |
| restart (server) | Neustart | „seit dem letzten Neustart des Servers“ |
| vacuum / autovacuum | VACUUM / Autovacuum | |
| dead rows | tote Zeilen | |
| bloat / empty space | Bloat / freier Speicher | label „Leerer Platz“ → „Freier Speicher“ |
| spill | Überlauf; spill to disk: auf den Datenträger auslagern | „tempdb-Überlauf“ |
| memory grant | Speicherzuweisung | |
| tempdb | tempdb | |
| buffer pool | Pufferpool | |
| cache / plan cache | Cache / Plancache | |
| CPU | CPU (die) | CPU-Zeit |
| memory | Arbeitsspeicher; short: Speicher | |
| I/O | I/O | |
| storage | Speicher (storage system: Speichersystem) | |
| worker (thread) | Worker-Thread | |
| parallelism | Parallelität | |
| cardinality estimate | Kardinalitätsschätzung | |
| parameter sniffing | Parameter Sniffing | |
| constraint | Einschränkung | untrusted: nicht vertrauenswürdig; not validated: nicht validiert |
| trigger | Trigger | |
| sample / observation | Stichprobe / Beobachtung | |
| window / period | Zeitfenster / Zeitraum | |
| history | Verlauf | |
| trend | Trend | |
| drift | Drift (die) | |
| advisor | Index-Ratgeber („Index advisor“) | |
| overview | Übersicht | |
| editor / tab | Editor / Tab | |
| result | Ergebnis | |
| environment | Umgebung: Produktion, Staging, Entwicklung, lokal | |
| production | Produktion | „auf Produktion“ |
| login | Anmeldung (the account: Login) | |
| password | Passwort | |
| source | Quelle | |
| capability locked | nicht verfügbar | never „gesperrt“: that is a lock |
| provider | Provider | |
| cloud | Cloud | |
| (service) tier | Dienstebene | Microsoft Learn |
| coding agent | Coding-Agent | |
| keychain | Schlüsselbund des Betriebssystems | |
| connection string | Verbindungszeichenfolge | Microsoft Learn |
| it / its (a query, a table) | name the noun again („Die Abfrage …“, „Die Tabelle …“) | a bare „Sie“/„Ihr“ reads as the reader |
| uptime | Betriebszeit | |
| planner, checkpoint, full-page image | Planer, Checkpoint, Full-Page-Image | |
| rows examined (MySQL) | untersuchte Zeilen | |
| replication lag, primary, subscription, consumer | Replikationsverzögerung, Primärserver, Subskription, Consumer | |
| batch, round trip, chatty queries | Batch, Roundtrip, „Abfragen mit zu vielen Roundtrips“ | |
| locking hint, row versioning | Sperrhinweis, Zeilenversionsverwaltung | |
| automatic tuning, compatibility level | automatische Optimierung, Kompatibilitätsgrad | |
| slow query log | Slow Query Log | |
| parameter sensitivity | Parameterempfindlichkeit | |
| hidden index (MySQL) | unsichtbar | |
| cleanup holder | Bereinigungsblocker | |
| query log (an app's) | Abfragelog | |
| insights (plan insights) | Erkenntnisse | |
| Right now (the waits tab), health | „Jetzt“, Zustand | |
| average (in a column) | Ø | |
| statement digests | Anweisungs-Digests | |
| gap / insert-intention lock | Gap-Sperre / Insert-Intention-Sperre | |
| shared / intent-exclusive / update / key-range lock | gemeinsame / beabsichtigte exklusive / Aktualisierungs- / Schlüsselbereichssperre | |
| schema stability / modification, bulk update lock | Schemastabilitäts- / Schemaänderungssperre, Massenaktualisierungssperre | |
| page life expectancy, version store | Seitenlebenserwartung, Versionsspeicher | |
| tenant / subscription (Azure) | Mandant / Abonnement | |
| burst balance, bastion host, continuation token | Burst-Guthaben, Bastion-Host, Fortsetzungstoken | |
| reset epoch, retention budget, request trace | Reset-Epoche, Aufbewahrungsbudget, Request-Trace | |
| insight (plan insight) | Erkenntnis | |
| sample (verb / noun) | erfassen / Stichprobe | „Jetzt erfassen“ |
| deploy, log (verb) | Deployment, protokollieren | |
| linked server, workload group | Verbindungsserver, Arbeitsauslastungsgruppe | Microsoft Learn |
| cost threshold for parallelism | Kostenschwellenwert für Parallelität | Microsoft Learn |
| tile (dashboard), collation, rebuild | Kachel, Kollation, neu erstellen | |
| terminations | Beendigungen | not „Abbrüche“ (cancellations) |
| a statement handle (`support.handle.*`) | nominative: „eine Leseabfrage auf {table}“, „ein UPDATE auf {table}“ | |
| `{window}` | accusative („die letzten 7 Tage“): „über {window}“, „umfasst {window}“ | |
| slow query log in a compound | Slow-Query-Log-Datei | standalone: Slow Query Log |

## UI verbs

Speichern, Abbrechen, Schließen, Kopieren, Kopiert, Löschen, Entfernen, Ausblenden (dismiss), Erneut versuchen,
Aktualisieren (refresh), Vergleichen, Teilen, Exportieren, Importieren, Öffnen, Verbinden, Trennen, Ausführen,
Messen, Markieren, Ignorieren, Suchen, Weiter, Zurück, Fertig, Leeren (clear), Bearbeiten, Umbenennen, Einfügen.
Progress: „Wird gespeichert…“, „Wird gelesen…“, „Wird ausgeführt…“.
