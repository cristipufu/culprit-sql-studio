# French glossary (`fr`)

How Culprit speaks French: read this before changing `locales/fr.json`, and add a term here before using a new one, so
every message says a thing the same way. The readers are developers and DBAs; write what a French-speaking DBA would
write, as concisely as the English.

## Voice

- Address the reader as **vous** (« Ouvrez-le dans l'éditeur », « Vous pouvez l'exécuter une fois »).
- Buttons and menu items are an **infinitive** verb, a word or two: Analyser, Annuler, Copier, Enregistrer, Exécuter,
  Mesurer, Ouvrir, Fermer, Supprimer, Réessayer, Actualiser, Exporter, Importer, Comparer, Se connecter.
- Hints and tooltips describe what happens, in the present: « Exécute l'instruction une fois ».
- Status lines in progress end with « … » as in English: « Lecture des vues de statistiques… ».
- Prefer short French over word-for-word: « Échec de la dernière analyse », not « La dernière analyse a échoué ».

## Capitalization

Sentence case everywhere (labels, headings, buttons, table columns, menu items): only the first word and proper nouns
take a capital. « Requêtes les plus coûteuses », not « Requêtes Les Plus Coûteuses ». Accented capitals keep their
accent: « État », « Événements étendus ».

## Typography

- Quotes are « guillemets » with a no-break space (U+00A0) inside: `Requête « {name} » enregistrée`.
- A no-break space (U+00A0) before `:`; a narrow no-break space (U+202F) before `;`, `!` and `?`.
- The apostrophe is typographic: `’` (U+2019).
- Keep `…` where English has it, and the middle dot `·` separators.
- Numbers, sizes, durations and percentages come formatted from code (`1 234,5`); a fixed number English writes
  (« 95 % », « 200 lignes ») keeps French spacing.
- Units: `o`, `Ko`, `Mo`, `Go`, `To`; counts `k`, `M`, `Md` after a space (« 1,2 k »); `h`, `min`, `s`, `ms`, `µs`;
  a narrow no-break space before `%` (« 23 % »).
- Keyboard keys are written as English shows them: `Enter`, `Esc`, `⌘`.

## Dates and periods

- A moment: « aujourd'hui à 12:13 », « hier à 09:02 », « 7 oct. à 12:13 »; « il y a 3 h », « à l'instant ».
- Two short moments from code (`{from}`, `{to}` from `when().short`) read « entre {from} et {to} », never « du {from}
  au {to} », which breaks on « aujourd'hui ». A full date (`when().full`, « samedi 8 novembre 2026 à 12:13 ») takes
  « du … au … ».
- A period phrase (`{window}`, `{span}` from `ui.rates.window.*` and `ui.format.window.*`) starts with its article
  (« les 7 derniers jours », « la période que couvre le journal… »): put it after « sur », « couvre » or a colon, never
  after « de » or « à », which would need « des » or « aux ». A bare length (`{span}` = « 7 jours », « 5 h ») has no
  gender to agree with: « sur {span} », « les {span} précédant l'analyse ».

## Plurals

French uses the CLDR `fr` cardinal rules: 0 and 1 are singular (« 0 ligne », « 1 ligne », « 2 lignes »), including
decimals whose integer part is 0 or 1. CLDR also defines `many` for whole millions. Every `plural`
whose words change with the number has a `one` case, even where English has only `other`. English's `=0` and `=1` cases
are always kept (they win over `one`).

## Kept in English

Engine and product names (SQL Server, PostgreSQL, MySQL, MariaDB, SQLite, YugabyteDB, Azure SQL, Entra ID, AWS, Azure,
VS Code, Culprit), SQL keywords (`SELECT`, `VACUUM`, `ANALYZE`, `ROLLBACK`), setting, view, wait type and operator names
as the engine shows them (`max_connections`, `PAGEIOLATCH_SH`, `pg_stat_statements`, `Index Seek`), feature names DBAs
say in English (Query Store, autovacuum, tempdb, buffer pool, DocDB, tablet, tablegroup, sharding, heap), and `CPU`.

## Terms

| English | French | Notes |
|---|---|---|
| finding | constat (m.) | « 3 constats » ; a finding's parts: titre, résumé, correctif, vérification |
| check | contrôle (m.) | what Culprit runs on a snapshot |
| scan (Culprit's) | analyse (f.) ; analyser | « Lancer une analyse », « Échec de l'analyse » |
| analysis | analyse (f.) | |
| report | rapport (m.) | |
| opportunity | opportunité (f.) | as performance tools say (Lighthouse) |
| observation | observation (f.) | |
| severity | gravité (f.) | critique, élevée, moyenne, faible, info |
| evidence | éléments (m. pl.) ; « preuves » only as a heading | « Éléments insuffisants » |
| fix (noun) | correctif (m.) | verb: corriger |
| verify | vérifier ; vérification (f.) | |
| caveat | réserve (f.) | |
| baseline | référence (f.) | « A est la référence » |
| snapshot | instantané (m.) | |
| workload | charge de travail (f.) | |
| query | requête (f.) | |
| statement | instruction (f.) | |
| table-valued parameter, bulk insert, array | paramètre table, insertion en masse, tableau | La syntaxe SQL reste inchangée. |
| batch | lot (m.) | |
| call | appel (m.) | |
| execution / run | exécution (f.) ; exécuter | |
| plan | plan (m.) d'exécution | « plan estimé », « plan réel » |
| actual (plan, rows) | réel | « nombre de lignes réel » |
| estimate | estimation (f.) ; estimé | |
| optimizer | optimiseur (m.) | |
| operator | opérateur (m.) | |
| step (of a plan) | étape (f.) | |
| wait | attente (f.) | |
| wait type | type d'attente (m.) | the name stays as the engine shows it |
| lock | verrou (m.) ; verrouiller | |
| lock escalation | escalade de verrous (f.) | |
| blocking | blocage (m.) ; bloquer | blocker: bloqueur ; blocked: bloqué |
| blocking chain | chaîne de blocage (f.) | |
| deadlock | interblocage (m.) | Microsoft Learn and the PostgreSQL docs |
| victim | victime (f.) | |
| index | index (m., invariable) | clustered: cluster ; nonclustered: non cluster |
| seek | recherche (f.) d'index | |
| scan (index, table) | parcours (m.) | « parcours d'index », « parcours séquentiel » (PostgreSQL docs) |
| lookup | recherche de clé (f.) | |
| missing / unused / duplicate index | index manquant / inutilisé / en double | |
| heap | heap (m.) | « table heap » when it helps: a table without a clustered index |
| forwarding pointer | pointeur de transfert (m.) | forwarded record: enregistrement transféré |
| table | table (f.) | |
| column | colonne (f.) | |
| row | ligne (f.) | |
| page | page (f.) | |
| constraint | contrainte (f.) | not validated: non validée ; untrusted: non approuvée |
| trigger | déclencheur (m.) | |
| read / write | lecture (f.) / écriture (f.) | reads from disk: lectures sur disque |
| latency | latence (f.) | |
| throughput | débit (m.) | |
| duration / elapsed | durée (f.) / durée écoulée | |
| regression | régression (f.) | |
| trend | tendance (f.) | |
| window (of time) | période (f.) | |
| sample | échantillon (m.) | |
| weight | poids (m.) | |
| connection | connexion (f.) ; se connecter | |
| login (SQL Server) | identifiant (m.) | not « connexion », which is a connection |
| sign in | se connecter | |
| session | session (f.) | |
| server | serveur (m.) | |
| database | base de données (f.), base (f.) | |
| setting | paramètre (m.) | app preferences: paramètres |
| statistics | statistiques (f. pl.) | statistics views: vues de statistiques |
| counter | compteur (m.) | reset: réinitialiser |
| restart | redémarrage (m.) ; redémarrer | |
| vacuum / autovacuum | VACUUM / autovacuum | as the PostgreSQL docs |
| dead rows | lignes mortes (f. pl.) | |
| bloat / empty space | espace vide (m.) ; bloat | |
| spill | débordement (m.) ; déborder | « a débordé sur disque » |
| memory grant | allocation de mémoire (f.) | « mémoire allouée » |
| grant (permission) | autorisation (f.) ; GRANT | |
| tempdb | tempdb | |
| buffer pool | buffer pool (m.) | |
| cache / plan cache | cache (m.) / cache des plans | |
| CPU | CPU (m.) | |
| memory | mémoire (f.) | |
| I/O | E/S (f. pl.) | Microsoft Learn |
| disk | disque (m.) | |
| worker (thread) | worker (m.) | parallel workers |
| parallelism | parallélisme (m.) | |
| compile / compilation | compilation (f.) | |
| thread scheduling | ordonnancement | attribution du CPU aux workers prêts à s’exécuter |
| transaction | transaction (f.) | commit: valider ; roll back: annuler |
| cancelled | annulé | timeout: délai d'attente dépassé |
| production | production (f.) | |
| environment | environnement (m.) | |
| replica / failover | réplica (m.) / basculement (m.) | |
| node / cluster | nœud (m.) / cluster (m.) | |
| dashboard | tableau de bord (m.) | |
| overview | vue d'ensemble (f.) | |
| advisor | conseiller (m.) | index advisor: conseiller d'index |
| drift | dérive (f.) | |
| editor | éditeur (m.) | |
| tab | onglet (m.) | |
| live | en direct | |
| schema | schéma (m.) | |
| source | source (f.) | |
| capability | capacité (f.) | locked: indisponible |
| mark (a finding) | marquer | dismissed: écarté ; resolved: résolu ; fixed: corrigé |
| copy / paste / drop | copier / coller / déposer | |
| share | partager ; share of time: part (f.) | |
| database time | temps de base de données | « temps BD » in a table column |
| uptime | temps de fonctionnement (m.) | |
| Explain / Analyze (editor buttons) | Expliquer / Mesurer | « Analyser » is Culprit's scan |
| measure (a run) | mesurer ; exécution mesurée | |
| request (HTTP, in an app log) | requête HTTP (f.) | « requête » alone is a query |
| latch | latch (m.) | « attentes de latch de page » |
| flush (log) | vidage (m.) | |
| version store | banque des versions (f.) | Microsoft Learn |
| staging | préproduction (f.) | |
| scratch tab | onglet de brouillon (m.) | |
| placeholder (`?`, `$1`) | paramètre fictif (m.) | |
| keychain | trousseau (m.) du système | |
| jump host | hôte de rebond (m.) | |
| coding agent | agent de code (m.) | |
| brief (for an agent) | synthèse (f.) | |
| timeline | chronologie (f.) | |
| chunk, hypertable, columnstore, rowstore | kept | TimescaleDB and SQL Server names |
| needs attention / worth a look | à traiter / à surveiller | status labels |
| checkpoint (the operation) | point de contrôle (m.) | Microsoft Learn, PostgreSQL FR; setting and view names stay |
| Health (the view), Right now (the waits scope) | Santé, En ce moment | |
| replication slot, standby, replay | slot de réplication, serveur standby, rejeu | |
| chatty (queries), heartbeat, poll | bavardes, signal de vie, guetter | |
| forced parameterization, hint, trace flag | paramétrage forcé, indicateur, indicateur de trace | |
| planner, automatic tuning, unforce | planificateur, réglage automatique, annuler le forçage | |
| full-page image, wraparound, freeze, backfill | image de page complète, bouclage, geler, remplissage rétroactif | |
| an application request, a SQL Server request | requête HTTP, demande | |
| (service) tier, row versioning | niveau de service, versionnage des lignes | |
| insight (a plan insight) | diagnostic (m.) | « Diagnostics du plan »; not *observation*, which is a sampled activity |
| slow query log, blocked process report, lock holder | journal des requêtes lentes, rapport de processus bloqués, détenteur | |
| linked server, join buffer, visibility map, index-only scan | serveur lié, tampon de jointure, carte de visibilité, parcours d’index seul | |
| running totals | totaux cumulés | |
| DB parameter group, database flags | groupe de paramètres de base de données, indicateurs de base de données | |
| Explain again | « Expliquez à nouveau » (« Relancez EXPLAIN ANALYZE » for PostgreSQL) | |
| tile (dashboard) | vignette (f.) | |
| a handle (`support.handle.*`) | starts with « une »/« un »: never put `{handle}` after « de » or « que » | no elision, unknown gender |
| tier (cloud), throttling, burst / baseline | niveau de service, limitation, burst / ligne de base | |
| endpoint, tenant / subscription, token | point de terminaison, locataire / abonnement, jeton | |
| CLI, bastion, SSH forwarding, timestamp | la CLI, bastion, redirection SSH, horodatage | |
| statement shapes, digest | formes, digest | |
| page life expectancy, batch requests | espérance de vie des pages, demandes de lots | |
| gap / insert-intention lock, intent lock | verrou d’intervalle / d’intention d’insertion, verrou intentionnel | |
| the Queries, Live views | Requêtes, En direct | |
