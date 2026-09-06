// Migration is dispatched explicitly through service::execute_stream so it
// shares the mutation cancellation and recovery lifetime. TUI entry is read-only.
