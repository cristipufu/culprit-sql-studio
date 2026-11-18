//! Each bounded DMV population is aggregated once. The cap+1 witness remains visible, and nullable/invalid native
//! values are counted before the summary. No ordering/sort, SQL text, plan, file path, new sampler or retry.
use super::{MAX_GRANT_REQUESTS, MAX_PENDING_IOS, MAX_SCHEDULERS, MAX_WAITING_TASKS};

pub(super) fn query() -> String {
    format!(
        r#"SELECT /* culprit */ CONVERT(varchar(40),SYSUTCDATETIME(),127)+'Z' AS observed_at,
 s.scheduler_population_rows,s.scheduler_rows,s.scheduler_invalid,s.runnable_tasks,s.worker_queue,s.failed_worker_creations,
 p.process_memory_rows,p.process_memory_invalid,p.process_physical_low,p.process_virtual_low,
 p.process_physical_used_kib,p.process_virtual_committed_kib,p.process_virtual_available_kib,
 m.system_memory_rows,m.system_memory_invalid,m.system_memory_low,m.system_total_kib,m.system_available_kib,
 g.grant_rows,g.grant_waiters,i.pending_io_rows,i.pending_io_invalid,i.os_pending_io,i.completion_pending_io,
 w.io_waiter_population_rows,w.io_waiter_rows,w.io_waiter_unknown_types,w.io_waiter_unknown_durations,w.longest_io_related_wait_ms
FROM (
 SELECT COUNT_BIG(*) AS scheduler_population_rows,
  COUNT_BIG(CASE WHEN eligible=1 THEN 1 END) AS scheduler_rows,
  COUNT_BIG(CASE WHEN status IS NULL OR status NOT IN(N'HIDDEN ONLINE',N'HIDDEN OFFLINE',N'VISIBLE ONLINE',N'VISIBLE OFFLINE',N'VISIBLE ONLINE (DAC)',N'HOT_ADDED') OR is_online IS NULL
    OR eligible=1 AND (runnable_tasks_count IS NULL OR runnable_tasks_count<0 OR work_queue_count IS NULL OR work_queue_count<0 OR failed_to_create_worker IS NULL) THEN 1 END) AS scheduler_invalid,
  CASE WHEN COUNT_BIG(CASE WHEN eligible=1 THEN runnable_tasks_count END)=COUNT_BIG(CASE WHEN eligible=1 THEN 1 END)
       THEN SUM(CASE WHEN eligible=1 THEN CONVERT(bigint,runnable_tasks_count) END) END AS runnable_tasks,
  CASE WHEN COUNT_BIG(CASE WHEN eligible=1 THEN work_queue_count END)=COUNT_BIG(CASE WHEN eligible=1 THEN 1 END)
       THEN SUM(CASE WHEN eligible=1 THEN CONVERT(bigint,work_queue_count) END) END AS worker_queue,
  CASE WHEN COUNT_BIG(CASE WHEN eligible=1 THEN failed_to_create_worker END)=COUNT_BIG(CASE WHEN eligible=1 THEN 1 END)
       THEN COUNT_BIG(CASE WHEN eligible=1 AND failed_to_create_worker=1 THEN 1 END) END AS failed_worker_creations
 FROM (SELECT TOP({schedulers}) status,is_online,runnable_tasks_count,work_queue_count,failed_to_create_worker FROM sys.dm_os_schedulers) native
 CROSS APPLY (SELECT CASE WHEN status=N'VISIBLE ONLINE' AND is_online=1 THEN 1 ELSE 0 END AS eligible) classification
) s
CROSS JOIN (
 SELECT COUNT_BIG(*) AS process_memory_rows,
  COUNT_BIG(CASE WHEN process_physical_memory_low IS NULL OR process_virtual_memory_low IS NULL THEN 1 END) AS process_memory_invalid,
  CASE WHEN COUNT_BIG(*)=1 THEN MAX(CONVERT(bigint,process_physical_memory_low)) END AS process_physical_low,
  CASE WHEN COUNT_BIG(*)=1 THEN MAX(CONVERT(bigint,process_virtual_memory_low)) END AS process_virtual_low,
  CASE WHEN COUNT_BIG(*)=1 THEN MAX(physical_memory_in_use_kb) END AS process_physical_used_kib,
  CASE WHEN COUNT_BIG(*)=1 THEN MAX(virtual_address_space_committed_kb) END AS process_virtual_committed_kib,
  CASE WHEN COUNT_BIG(*)=1 THEN MAX(virtual_address_space_available_kb) END AS process_virtual_available_kib
 FROM (SELECT TOP(2) process_physical_memory_low,process_virtual_memory_low,physical_memory_in_use_kb,
       virtual_address_space_committed_kb,virtual_address_space_available_kb FROM sys.dm_os_process_memory) native
) p
CROSS JOIN (
 SELECT COUNT_BIG(*) AS system_memory_rows,
  COUNT_BIG(CASE WHEN system_low_memory_signal_state IS NULL THEN 1 END) AS system_memory_invalid,
  CASE WHEN COUNT_BIG(*)=1 THEN MAX(CONVERT(bigint,system_low_memory_signal_state)) END AS system_memory_low,
  CASE WHEN COUNT_BIG(*)=1 THEN MAX(total_physical_memory_kb) END AS system_total_kib,
  CASE WHEN COUNT_BIG(*)=1 THEN MAX(available_physical_memory_kb) END AS system_available_kib
 FROM (SELECT TOP(2) system_low_memory_signal_state,total_physical_memory_kb,available_physical_memory_kb FROM sys.dm_os_sys_memory) native
) m
CROSS JOIN (
 SELECT COUNT_BIG(*) AS grant_rows,COUNT_BIG(CASE WHEN grant_time IS NULL THEN 1 END) AS grant_waiters
 FROM (SELECT TOP({grants}) grant_time FROM sys.dm_exec_query_memory_grants) native
) g
CROSS JOIN (
 SELECT COUNT_BIG(*) AS pending_io_rows,
  COUNT_BIG(CASE WHEN io_pending IS NULL OR io_pending NOT IN(0,1) THEN 1 END) AS pending_io_invalid,
  CASE WHEN COUNT_BIG(CASE WHEN io_pending IN(0,1) THEN 1 END)=COUNT_BIG(*) THEN COUNT_BIG(CASE WHEN io_pending=1 THEN 1 END) END AS os_pending_io,
  CASE WHEN COUNT_BIG(CASE WHEN io_pending IN(0,1) THEN 1 END)=COUNT_BIG(*) THEN COUNT_BIG(CASE WHEN io_pending=0 THEN 1 END) END AS completion_pending_io
 FROM (SELECT TOP({ios}) io_pending FROM sys.dm_io_pending_io_requests) native
) i
CROSS JOIN (
 SELECT COUNT_BIG(*) AS io_waiter_population_rows,
  COUNT_BIG(CASE WHEN io_related=1 THEN 1 END) AS io_waiter_rows,
  COUNT_BIG(CASE WHEN wait_type IS NULL OR LEN(wait_type)=0 THEN 1 END) AS io_waiter_unknown_types,
  COUNT_BIG(CASE WHEN wait_duration_ms IS NULL OR wait_duration_ms<0 THEN 1 END) AS io_waiter_unknown_durations,
  MAX(CASE WHEN io_related=1 THEN wait_duration_ms END) AS longest_io_related_wait_ms
 FROM (SELECT TOP({waits}) wait_type,wait_duration_ms FROM sys.dm_os_waiting_tasks) native
 CROSS APPLY (SELECT CASE WHEN wait_type IN(N'PAGEIOLATCH_SH',N'PAGEIOLATCH_EX',N'PAGEIOLATCH_UP',N'IO_COMPLETION',N'ASYNC_IO_COMPLETION',N'WRITELOG') THEN 1 ELSE 0 END AS io_related) classification
) w OPTION(MAXDOP 1)"#,
        schedulers = MAX_SCHEDULERS + 1,
        grants = MAX_GRANT_REQUESTS + 1,
        ios = MAX_PENDING_IOS + 1,
        waits = MAX_WAITING_TASKS + 1
    )
}
