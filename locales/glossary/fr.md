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
- Numbers, sizes, durations and percentages come formatted from code (`1 234,5`): never write digits into a message.
- Keyboard keys are written as English shows them: `Enter`, `Esc`, `⌘`.

## Plurals

French's plural rule is `one_below_2`: 0 and 1 are singular (« 0 ligne », « 1 ligne », « 2 lignes »). Every `plural`
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
| database time | temps de base de données | |
| uptime | temps de fonctionnement (m.) | |
