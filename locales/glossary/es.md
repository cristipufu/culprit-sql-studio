# Spanish (`es`) glossary

How Culprit speaks Spanish. Read this before changing `locales/es.json`, and add a term here before using it in a new way.

## Voice

- **Address**: *tú*, as Microsoft's Spanish style guide does: «Pega una cadena de conexión», «Elige el archivo». Never
  *usted*, never *vosotros*.
- **Variety**: neutral international Spanish. No regionalisms (*ordenador*/*computadora*: say *equipo*; *coger*: say
  *tomar*, *elegir*); *costo*, not *coste*.
- **Register**: what a Spanish-speaking DBA writes to a colleague: direct, precise, as short as the English. Don't
  translate word for word; rebuild the sentence.

## Capitalization

Sentence case everywhere: buttons, menus, headings, table columns, tabs. Only the first word and proper nouns take a
capital: «Asesor de índices», «Abrir en Consultas», «Esperas de bloqueo». A view's name is a proper noun when it names the
place in the app (*Consultas*, *Planes*, *Hallazgos*, *En vivo*, *Esquema*, *Panel*), so it keeps its capital inside a
sentence: «Abrir en Planes».

## Buttons, menus and commands

- Buttons and menu items take the **infinitive**: *Guardar*, *Cancelar*, *Copiar*, *Ejecutar*, *Descartar*, *Volver a
  escanear*. Instructions in a sentence take the **imperative with tú**: «Pega aquí», «Elige otro…», «Vuelve a escanear».
- Microsoft's verbs: *Quitar* (remove from a list, a connection), *Eliminar* only for destroying data; *Borrar* (clear a
  field or filter); *Actualizar* (refresh a view; also install an update); *Restaurar* (restore a dismissed finding);
  *Descartar* (dismiss); *Probar* (test); *Importar*; *Pausar*/*Reanudar*; *Mantener* (keep, in a confirmation).
- Keys as English shows them: Enter, Esc, ⌘, Tab, {key}.

## Typography

- Opening **¿** and **¡** always.
- Quotes: **« »** (comillas latinas), everywhere a message quotes a name or a phrase: «{name}».
- No space before **: ; ! ?** (Spanish never puts one). A space after.
- **…** where English has it, as the single character.
- Percent: `{value} %` with a no-break space (U+00A0) before %, as the Spanish norm and CLDR write it. In prose a
  percentage takes its article (*ocupa el {share} del tiempo de base de datos*, *al {share}*); labels, chips and table
  cells don't (*{share} del tiempo de BD*). Units keep their symbol: *B, KB, MB, GB, TB, ms, s, min, h, µs*.
- A count passed as plain text, with no plural argument, can't make a noun agree: put the noun first (*alto: {count}*,
  *más lentas: {count}*) or use a phrase that doesn't inflect (*{still} sin corregir*).
- Compact counts: *mil* (thousand), *M* (million), *mil M* (thousand million: Spanish *billón* is 10¹², never use it).
- Dates read *hoy, 14:30* / *ayer, 9:02*: a comma, never *a las* (it would be *a la 1:05*). UTC times from the core are
  always two-digit (*13:58*), so they may take *a las*: *el 11 oct a las 13:58 UTC*.
- Day before month, months as lowercase abbreviations: *11 oct*, *11 oct 2026*; *ene, feb, mar, abr, may, jun, jul,
  ago, sept, oct, nov, dic*.
- A period with its length (*the last {span}*, where *{span}* may be days, hours or minutes) is *el último periodo de
  {span}*, since *los últimos* / *las últimas* would have to agree with a noun the message doesn't know.
- Ordinals: *1.ª*, *2.ª* (they count keys: *la clave*).

## Kept in English

Engine and product names (SQL Server, PostgreSQL, MySQL, MariaDB, SQLite, YugabyteDB, DocDB, TimescaleDB, Aurora, RDS,
Azure, AWS, Entra ID, VS Code, DBeaver, DataGrip, SSMS, OpenSSH, Rails, Django), Culprit's own names (Culprit,
`culprit mcp`), SQL keywords (SELECT, UPDATE, WHERE, CREATE INDEX, EXPLAIN, AFTER/BEFORE), setting, view and wait type
names (`max_connections`, `track_io_timing`, `log_lock_waits`, `busy_timeout`, PAGEIOLATCH_SH, `pg_stats`,
`system_health`), file formats and acronyms (JSON, HTML, Markdown, .sqlplan, DDL, OLTP, JIT, WAL, HOT, LSM, RPC, IAM,
SSH, PK, FK, CPU), operator names as the engine prints them (Index Seek, Seq Scan, Key Lookup) and *pull request*,
*Query Store*, *tempdb*, *hypertable*, *chunk*, *tablet*, *tablegroup*, *slot* (replication slot), *latch*, *trigger*,
*vacuum*/*autovacuum*.

## Terms

Decided once; use the same word everywhere. Where Microsoft Learn (SQL Server) and the PostgreSQL translation differ,
the choice and the reason are given.

| English | Spanish | Notes |
|---|---|---|
| finding | hallazgo (m) | *Hallazgos* is the view's name |
| check (Culprit's) | comprobación (f) | *Cobertura de comprobaciones* |
| scan (Culprit's collection), to scan | escaneo (m), escanear | *Volver a escanear*, *escaneado hace 3 días* |
| scan (an operator reading a whole table or index) | recorrido (m) | *recorrido completo*, *recorrido de índice*; the engine's operator name stays English (Index Scan, Seq Scan) |
| read in full / full scan | leer entera / recorrido completo | |
| seek | búsqueda (f) | Microsoft: *Index Seek* = búsqueda de índice; *buscar directamente* where a verb is needed |
| lookup / key lookup | búsqueda de clave (f) | Microsoft |
| report | informe (m) | |
| workload | carga de trabajo (f) | |
| query | consulta (f) | |
| statement | instrucción (f) | Microsoft (*instrucción SELECT*); never *sentencia* |
| run (one execution), to run | ejecución (f), ejecutar | *ejecuciones/día*, *por ejecución* |
| plan, estimated / actual / generic plan | plan, plan estimado / real / genérico | |
| operator | operador (m) | |
| step (of a plan) | paso (m) | |
| optimizer | optimizador (m) | |
| estimate | estimación (f) | |
| cost, subtree cost | costo, costo del subárbol | Microsoft |
| predicate, filter | predicado, filtro | |
| wait, wait type | espera (f), tipo de espera | |
| lock, to lock | bloqueo (m), bloquear | Microsoft |
| blocking, blocked, blocking chain | bloqueo, bloqueada (la sesión), cadena de bloqueo | Microsoft also says *bloqueo*; *espera de bloqueo* for a lock wait |
| lock escalation | escalado de bloqueos (m) | Microsoft |
| latch | latch (m), pl. latches | Microsoft says *bloqueo temporal*; kept in English so it never reads as a lock, and because the wait types are PAGELATCH_* |
| deadlock | interbloqueo (m) | Microsoft and the PostgreSQL translation |
| victim (deadlock) | víctima (f) | |
| index, clustered / nonclustered | índice, agrupado / no agrupado | Microsoft |
| included column, key column | columna incluida, columna de clave | Microsoft |
| missing index | índice que falta | Microsoft (*índices que faltan*) |
| heap (SQL Server table without a clustered index) | montón (m) | Microsoft; PostgreSQL's own storage stays *heap* |
| forwarded record, forwarding pointer | registro reenviado, puntero de reenvío | Microsoft |
| primary key, foreign key | clave primaria, clave foránea | the PostgreSQL and MySQL translations and common usage (Microsoft says *principal*/*externa*); PK, FK unchanged |
| constraint, untrusted, not validated | restricción, no confiable, no validada | |
| table, row, column, page | tabla, fila, columna, página | |
| dead rows | filas muertas | |
| read, write (noun/verb) | lectura, escritura / leer, escribir | |
| flush (log) | vaciado (m) | Microsoft |
| latency, throughput | latencia, rendimiento | |
| regression | regresión (f) | |
| opportunity | oportunidad (f) | |
| severity / impact | gravedad / impacto | the app says impact: *Impacto alto* |
| evidence | evidencia (f) | |
| fix (noun), to fix | corrección (f), corregir | *Verificar la corrección* |
| verify | verificar | *Cómo verificarlo* |
| caveat | *Antes de actuar* (heading) | |
| baseline | línea base (f) | Microsoft |
| snapshot | instantánea (f) | |
| connection, connection string | conexión, cadena de conexión | |
| session, login | sesión, inicio de sesión | Microsoft |
| server, database, schema | servidor, base de datos (BD), esquema | *tiempo de BD* where English says *DB time* |
| database time | tiempo de base de datos | |
| setting, settings | ajuste (m), configuración (f) | one setting is an *ajuste*; the group is *Configuración* |
| statistics, counters, reset | estadísticas, contadores, restablecer | Microsoft: *restablecer* |
| transaction, commit, roll back | transacción, confirmar, revertir | Microsoft; *revertida* (rolled back) |
| idle, idle in transaction | inactiva, inactiva en una transacción | of a session |
| spill (to disk) | volcado (a disco), volcarse a disco | |
| memory grant | concesión de memoria | Microsoft |
| buffer pool, cache, plan cache | grupo de búferes, caché (f), caché de planes | Microsoft |
| version store | almacén de versiones | Microsoft |
| CPU, memory, I/O | CPU, memoria, E/S | Microsoft writes *E/S* |
| worker thread | subproceso de trabajo | Microsoft; *subproceso* for short |
| compile, compilation | compilar, compilación | |
| log (transaction log, application log) | registro (m) | *registro de transacciones*; an application's log: *el registro de Rails* |
| replica, replication, replication slot | réplica, replicación, slot de replicación | |
| fill factor | factor de relleno | the setting `fillfactor` stays as is |
| columnstore, rowstore | almacén de columnas, almacén de filas | Microsoft |
| histogram | histograma (m) | |
| sample, sampling | muestra, muestreo | |
| trend | tendencia (f) | |
| share (of a total) | parte / porcentaje | *{share} del tiempo de BD* |
| dashboard | panel (m) | |
| advisor, index advisor | asesor, asesor de índices | |
| drift (schema) | diferencias | |
| mark (on the timeline) | marca (f), marcar | |
| deploy, migration | despliegue (m), migración (f) | |
| production, staging, development | producción, preproducción, desarrollo | |
| environment | entorno (m) | |
| coding agent | agente de programación | |
| keychain | llavero del sistema | |
| computer | equipo (m) | Microsoft |
| tab | pestaña (f) | |
| clipboard | portapapeles (m) | |
| screenshot | captura de pantalla | |
| command palette | paleta de comandos | |
| jump host, identity file | host de salto, archivo de identidad | |
| brief (for an agent) | resumen (m) | |
| node, cluster | nodo, clúster | |
| throttling (cloud) | limitación (f) | |
| tier | nivel (m) | |
| join | combinación (f) | Microsoft; JOIN in SQL stays |
| checkpoint, full-page image | punto de control, imagen de página completa | Microsoft and the PostgreSQL translation |
| slow query log | registro de consultas lentas | |
| connection pool, driver | pool de conexiones, driver | |
| insight (plan insights) | observación (f) | *Observaciones del plan*; never *hallazgo*, which is a finding |
| health (the view, the tab) | estado (m) | *Estado de la base de datos*; a healthy signal is *en buen estado* |
| upsert, get-or-create | upsert, «obtener o crear» | |
| explain (a statement) | obtener el plan (EXPLAIN) | the *Explicar* button keeps the verb |
| insert, update, delete, merge (a write) | inserción, actualización, eliminación, MERGE | *una lectura de {table}*, *una eliminación en {table}* |
| backfill | relleno de datos | |
| hot pages, hot rows | páginas muy disputadas, filas muy disputadas | |
