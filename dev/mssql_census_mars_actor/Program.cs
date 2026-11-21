using Microsoft.Data.SqlClient;
using System.Data;
using System.Text.Json;

// Fixture-only. Rust admits the existing nonce owner and independently checks native MARS rows.
internal static class Program
{
    private const int SetupSeconds = 5;
    private const int WorkSeconds = 30;
    private const int WholeActorSeconds = 45;
    private const int HeldRows = 4096;
    private const int HeldPayloadBytes = 4096;
    private const string SixteenValues = "(VALUES(0),(1),(2),(3),(4),(5),(6),(7),(8),(9),(10),(11),(12),(13),(14),(15))";
    private const string IdentifySql = "SELECT /* culprit census actor identity */ CONVERT(varchar(16),s.session_id),CONVERT(varchar(40),CAST(s.login_time AS datetime2),126),DB_NAME(),CONVERT(nvarchar(128),SERVERPROPERTY('MachineName')) FROM sys.dm_exec_sessions s WHERE s.session_id=@@SPID";
    // MARS interleaves yielding SELECT results. WAITFOR can prevent the second request from starting:
    // https://learn.microsoft.com/en-us/sql/connect/ado-net/sql/enable-multiple-active-result-sets
    // Each held result is at most 4096 rows of 4096 payload bytes, on the disposable fixture only.
    private static readonly string HoldSql = $"SELECT TOP ({HeldRows}) /* culprit census actor held output */ 1 AS started,REPLICATE(CAST('x' AS varchar({HeldPayloadBytes})),{HeldPayloadBytes}) AS payload FROM {SixteenValues} a(n) CROSS JOIN {SixteenValues} b(n) CROSS JOIN {SixteenValues} c(n) OPTION(MAXDOP 1)";
    private static readonly string ForeignSql = "EXEC /* culprit census actor foreign */ master.sys.sp_executesql N'" + HoldSql.Replace("'", "''") + "';";

    private static async Task<int> Main()
    {
        var started = System.Diagnostics.Stopwatch.StartNew();
        var ready = Required("CULPRIT_CENSUS_MARS_READY");
        var completed = Required("CULPRIT_CENSUS_MARS_COMPLETED");
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(WholeActorSeconds));
        await using var mars = new SqlConnection(ConnectionString("mars"));
        await using var foreign = new SqlConnection(ConnectionString("foreign"));
        var commands = new List<SqlCommand>();
        var readers = new List<SqlDataReader>();
        Exception? failure = null;
        var queries = 0;
        var closed = false;
        try
        {
            await mars.OpenAsync(deadline.Token);
            await foreign.OpenAsync(deadline.Token);
            queries++;
            var original = await Identify(mars, deadline.Token);
            for (var index = 0; index < 2; index++)
            {
                var command = new SqlCommand(HoldSql, mars) { CommandTimeout = WorkSeconds + SetupSeconds };
                commands.Add(command);
                queries++;
                var reader = await command.ExecuteReaderAsync(CommandBehavior.SequentialAccess, deadline.Token);
                readers.Add(reader);
                if (!await reader.ReadAsync(deadline.Token) || reader.GetInt32(0) != 1) throw new InvalidOperationException();
            }
            var foreignCommand = new SqlCommand(ForeignSql, foreign) { CommandTimeout = WorkSeconds + SetupSeconds };
            commands.Add(foreignCommand);
            queries++;
            var foreignReader = await foreignCommand.ExecuteReaderAsync(CommandBehavior.SequentialAccess, deadline.Token);
            readers.Add(foreignReader);
            if (!await foreignReader.ReadAsync(deadline.Token) || foreignReader.GetInt32(0) != 1) throw new InvalidOperationException();
            // Complete a separate request in the original database while master output remains held.
            // The Rust witness requires both native database IDs; the fixture never assumes them.
            queries++;
            var foreignOriginal = await Identify(foreign, deadline.Token);
            Write(ready, new { state = "ready", mars = original, foreign = foreignOriginal, actor_queries = queries, work_seconds = WorkSeconds });
            // Retain the unread original streams until the bounded observation interval ends. Draining
            // either immediately would let that request finish before the independent native witness.
            await HoldObservationWindow(deadline.Token);
            foreach (var reader in readers) await Drain(reader, deadline.Token);
            Write(Required("CULPRIT_CENSUS_MARS_IDLE"), new { state = "idle", actor_queries = queries });
            // Keep the same drained connections open; the caller owns the last census and release.
            while (!File.Exists(Required("CULPRIT_CENSUS_MARS_RELEASE")))
                await Task.Delay(10, deadline.Token);
        }
        catch (Exception error)
        {
            failure = error;
            await deadline.CancelAsync();
        }
        finally
        {
            foreach (var reader in readers)
            {
                try { await reader.DisposeAsync(); } catch (Exception error) { failure ??= error; }
            }
            foreach (var command in commands)
            {
                try { await command.DisposeAsync(); } catch (Exception error) { failure ??= error; }
            }
            try { await mars.CloseAsync(); await foreign.CloseAsync(); closed = true; }
            catch (Exception error) { failure ??= error; }
        }
        var nativeCode = (failure as SqlException)?.Number;
        if (!File.Exists(ready)) Write(ready, new { state = "failed", native_code = nativeCode });
        Write(completed, new { completed = failure is null && closed, closed, actor_queries = queries,
            actor_elapsed_ms = started.Elapsed.TotalMilliseconds, whole_actor_seconds = WholeActorSeconds,
            work_seconds = WorkSeconds, native_code = nativeCode, error_type = failure?.GetType().Name });
        return failure is null && closed ? 0 : 1;
    }

    private static string ConnectionString(string role)
    {
        var owner = Required("CULPRIT_SMOKE_OWNER");
        var database = Required("CULPRIT_SMOKE_DATABASE");
        var port = int.Parse(Required("CULPRIT_MSSQL_SOURCE_TEST_PORT"));
        if (owner.Length != 24 || owner.Any(value => !char.IsAsciiHexDigit(value) || char.IsUpper(value))
            || database != "culprit_smoke_" + owner || port is <= 0 or > 65535) throw new InvalidOperationException();
        return new SqlConnectionStringBuilder { DataSource = "127.0.0.1," + port, InitialCatalog = database,
            UserID = "sa", Password = Required("CULPRIT_MSSQL_SOURCE_TEST_PASSWORD"), Pooling = false,
            // Each request gets its own execution environment. In the foreign control this keeps
            // the original session database separate from the active master request's database.
            MultipleActiveResultSets = true, TrustServerCertificate = true, ConnectTimeout = SetupSeconds,
            ApplicationName = "culprit-census-" + role + "-" + owner }.ConnectionString;
    }
    private static async Task<object> Identify(SqlConnection owner, CancellationToken cancellation)
    {
        await using var command = new SqlCommand(IdentifySql, owner) { CommandTimeout = SetupSeconds };
        await using var reader = await command.ExecuteReaderAsync(cancellation);
        if (!await reader.ReadAsync(cancellation)) throw new InvalidOperationException();
        var session = reader.GetString(0);
        var generation = reader.GetString(1);
        if (reader.GetString(2) != Required("CULPRIT_SMOKE_DATABASE")
            || reader.GetString(3) != "m" + Required("CULPRIT_SMOKE_OWNER")[..14]
            || await reader.ReadAsync(cancellation) || await reader.NextResultAsync(cancellation)) throw new InvalidOperationException();
        return new { session, generation };
    }
    private static async Task Drain(SqlDataReader reader, CancellationToken cancellation)
    {
        do { while (await reader.ReadAsync(cancellation)) { } } while (await reader.NextResultAsync(cancellation));
    }
    private static async Task HoldObservationWindow(CancellationToken cancellation)
    {
        var held = System.Diagnostics.Stopwatch.StartNew();
        var release = Required("CULPRIT_CENSUS_MARS_RELEASE");
        while (held.Elapsed < TimeSpan.FromSeconds(WorkSeconds) && !File.Exists(release))
            await Task.Delay(10, cancellation);
    }
    private static string Required(string name) => Environment.GetEnvironmentVariable(name) ?? throw new InvalidOperationException();
    private static void Write(string path, object value)
    {
        var pending = path + ".pending";
        using (var file = new FileStream(pending, FileMode.CreateNew, FileAccess.Write, FileShare.None))
        {
            JsonSerializer.Serialize(file, value);
            file.Flush(true);
        }
        if (!OperatingSystem.IsWindows()) File.SetUnixFileMode(pending, UnixFileMode.UserRead | UnixFileMode.UserWrite);
        File.Move(pending, path, false);
    }
}
