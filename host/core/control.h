#ifndef HAMN_CONTROL_H
#define HAMN_CONTROL_H

/* Call only in a fresh, single-threaded worker process. The caller owns the
 * returned UTF-8 JSON and must release it with hamn_control_free().
 *
 * A call that needs the stored configuration of its profile and cannot read
 * it changes nothing in the profile and says why through log_last_error():
 * "profile <name> does not exist" when there is no config.yaml, and otherwise
 * "cannot read the configuration of profile <name>: <reason>" with the rule
 * the file breaks (core/profile.h). */

/* The status of profile, or the list of all profiles when profile is NULL.
 * Returns 0 with *result owning the JSON, or -1 with *result NULL. The call
 * first forgets the reason an earlier call recorded. After -1,
 * log_last_error() holds the sentence above for a profile that cannot be
 * read and the complaint about a state file that cannot be read; for any
 * other failure it is empty and errno holds the error. The sentence is
 * recorded without being printed. */
int hamn_control_query(const char *profile, char **result);
void hamn_control_free(char *result);
int hamn_control_start(const char *profile, unsigned cpus,
                       unsigned memory_gib, unsigned disk_gib);
/* A zero resource keeps the profile's value. rosetta is 1 or 0 to set Apple
 * Linux Rosetta translation, -1 to keep it; any other value is invalid.
 * Returns 0, 2 for invalid arguments, 4 when create finds that the profile
 * exists, and 1 for any other failure. */
int hamn_control_configure(const char *profile, unsigned cpus,
                           unsigned memory_gib, unsigned disk_gib, int create,
                           int rosetta);
/* Make the stored configuration of profile equal to the profile definition
 * file at path: create the profile when it does not exist, replace
 * config.yaml when its settings differ, and do nothing when they are equal.
 * The definition is the whole configuration (core/profile.h describes it)
 * and must name profile. Never starts, stops or signals a VM. profile and
 * path are borrowed NUL-terminated strings; path names a regular file of at
 * most 65536 bytes and is resolved against the working directory. dry_run
 * is a boolean: when set, the call reports what it would do and creates,
 * writes and locks nothing, and does not look at the VM.
 *
 * Returns 0 with *result owning the UTF-8 JSON object {"profile", "action",
 * "changes", "dryRun"}; release it with hamn_control_free. action is
 * "create", "configure" or "none". changes lists the settings that a
 * "configure" replaces: {"key", "from", "to"} for a number or a boolean and
 * {"key"} alone for docker.daemonJson, mounts and provision. Otherwise
 * *result is NULL, log_last_error() says why, and the value is:
 *   2    the arguments, the file or the definition are refused; nothing
 *        under ~/.hamn was created or changed, not even a lock file;
 *   4    the state of the profile forbids the change: its VM is running, it
 *        is deleted, its directory holds other files but no config.yaml, or
 *        the disk would shrink;
 *   130  the call was cancelled while it waited for the profile's lifecycle
 *        lock, and gave up before any write once it held the lock;
 *   1    any other failure;
 *   5    the call cannot say what config.yaml holds: the write was not
 *        confirmed durable, the file could not be read back after a failed
 *        write, or the result could not be built after the write. The file
 *        can hold the new settings.
 * After 1, 2, 4 and 130 config.yaml holds what it held before. A directory
 * without config.yaml that holds nothing but what an interrupted or refused
 * creation leaves (the temporary file of a configuration write; the
 * operation record, state file and port forwarding locks of a first start)
 * counts as no profile, and the profile is created in it.
 *
 * Equal settings need no lock: such a call succeeds while the VM runs or
 * another operation holds the profile. A change takes the lifecycle lock,
 * then the mutation lock, and requires a VM that is known to be stopped. */
int hamn_control_apply(const char *profile, const char *path, int dry_run,
                       char **result);
int hamn_control_stop(const char *profile);
int hamn_control_delete(const char *profile);
int hamn_control_diagnostics(const char *profile, const char *path, char **result);
/* manifest is optional borrowed NUL-terminated UTF-8. check_only and force are
 * booleans and cannot both be true. Result owns at most 16 KiB of UTF-8 JSON;
 * release with hamn_control_free. Call in the fresh core worker only. Checks do
 * not acquire payloads or repair journals. Mutations require a managed symlink. */
int hamn_control_upgrade(const char *manifest, int check_only, int force,
                         char **result);
int hamn_control_uninstall(int confirmed);
/* The build's release version: a static NUL-terminated ASCII string, valid for
 * the whole process. Unlike the calls above, safe from any process or thread. */
const char *hamn_version(void);

#endif
