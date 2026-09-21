#ifndef HAMN_CONTROL_H
#define HAMN_CONTROL_H

/* Call only in a fresh, single-threaded worker process. The caller owns the
 * returned UTF-8 JSON and must release it with hamn_control_free(). */
int hamn_control_query(const char *profile, char **result);
void hamn_control_free(char *result);
int hamn_control_start(const char *profile, unsigned cpus,
                       unsigned memory_gib, unsigned disk_gib);
int hamn_control_configure(const char *profile, unsigned cpus,
                           unsigned memory_gib, unsigned disk_gib, int create);
int hamn_control_stop(const char *profile);
int hamn_control_delete(const char *profile);
int hamn_control_migrate(const char *profile);
int hamn_control_diagnostics(const char *profile, const char *path, char **result);
int hamn_control_update(const char *manifest);
/* manifest is optional borrowed NUL-terminated UTF-8. check_only and force are
 * booleans and cannot both be true. Result owns at most 16 KiB of UTF-8 JSON;
 * release with hamn_control_free. Call in the fresh core worker only. Checks do
 * not acquire payloads or repair journals. Mutations require a managed symlink. */
int hamn_control_upgrade(const char *manifest, int check_only, int force,
                         char **result);
int hamn_control_uninstall(int confirmed);

#endif
