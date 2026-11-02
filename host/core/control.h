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
 * recorded without being printed.
 *
 * The list has one object per directory under ~/.hamn that holds a
 * config.yaml and no marker of a deleted profile. A profile whose
 * config.yaml cannot be read does not fail the list: its object holds only
 * "name", "state" (of its VM, which can run), "directory" and
 * "configurationError", the reason. The list still fails as a whole when
 * ~/.hamn or a profile directory is not this user's alone, when a profile
 * directory cannot be entered, when a state file or a deletion marker is
 * not what it should be, and when memory or descriptors run out.
 *
 * The status of one profile changes nothing. The list is not a pure read:
 * before it lists, the call permanently deletes, with its disk and Docker
 * data, each profile that vm up made (one with a workspace record) whose
 * recorded workspace path no longer exists and whose VM is known to be
 * stopped (workspace_sweep in core/workspace.h); then it removes what
 * interrupted purges left (purge_sweep in core/purge.h). It changes no other
 * profile, and neither step ever fails the list. Callers still treat the
 * list as a query that needs no confirmation. */
int hamn_control_query(const char *profile, char **result);
void hamn_control_free(char *result);
/* Start the VM of profile, or bring a running one back to ready. A zero
 * resource keeps the profile's value. Returns 0; 2 for invalid arguments;
 * HAMN_CONTROL_GUEST_FILESYSTEM_DAMAGED when the guest root filesystem is
 * damaged (core/guest_filesystem.h): the start wrote nothing in the guest,
 * stopped again a VM it booted and left a VM that was running as it was,
 * and the status reports the damage as guestFilesystem; 130 when cancelled
 * with the VM restored; and 1 or -1 for any other failure, after which the
 * operation record says whether the outcome is known. A damage found while
 * the VM this start booted could not be stopped is such a failure. An
 * earlier operation whose outcome is unknown stays unknown in the record
 * after any failure.
 *
 * A start that would launch a VM first asks admission (core/admission.h)
 * whether the host can take it, before it changes the VM, its configuration
 * or its disk. HAMN_CONTROL_ADMISSION_REFUSED: the host could not, nothing
 * was started, *refusal owns the refusal JSON that admission_admit
 * describes (release it with hamn_control_free), and log_last_error() says
 * the same in a sentence. refusal must not be NULL; *refusal is NULL after
 * every other return. */
#define HAMN_CONTROL_GUEST_FILESYSTEM_DAMAGED 6
#define HAMN_CONTROL_ADMISSION_REFUSED 7
int hamn_control_start(const char *profile, unsigned cpus,
                       unsigned memory_gib, unsigned disk_gib,
                       char **refusal);
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
/* Delete profile permanently: stop its VM, then remove ~/.hamn/<profile>
 * with its disk, configuration, keys, logs and Docker data, as
 * core/purge.h describes. Shared host directories are not touched. A
 * soft-deleted profile is purged all the same, and a directory without
 * config.yaml is purged as a profile with the default settings. Every call
 * first removes what interrupted purges left (purge_sweep). profile is a
 * borrowed NUL-terminated string; absent is written on every return.
 *
 * Returns 0 once the directory is gone, with *absent 1 when there was no
 * ~/.hamn/<profile> to begin with: such a call creates, locks and changes
 * nothing, so a repeated purge has no effect. Otherwise *absent is 0,
 * log_last_error() says why, and the value is:
 *   2  profile is not a profile name, or absent is NULL;
 *   HAMN_CONTROL_STOP_UNCERTAIN  the VM or its host port forwards are not
 *      known to be stopped: nothing was deleted and the state of the VM is
 *      unknown, as when vm delete refuses for the same reason;
 *   1  any other failure. The configuration cannot be read (the sentences
 *      of the note above), another mutation holds the profile, or its
 *      directory could not be moved aside: nothing was deleted. Or the
 *      directory was moved aside and not entirely removed: the profile no
 *      longer exists, and the next vm list or purge removes what is left.
 *
 * Takes the lifecycle lock, waiting for it, then the mutation lock, which
 * it does not wait for. Both lock files stay, empty and reused by a later
 * profile of the same name: ~/.hamn/.locks/<profile>.lock and
 * ~/.hamn/.<profile>-mutation.lock. A process locks the file it opened, and
 * neither lock checks afterwards that the path still names that file. If a
 * purge removed a lock file while another process waited on it, that
 * process would get the removed file while a third created and locked a
 * new one at the same path, and both would own the profile. Released Hamn
 * binaries, which an open TUI can keep running after an upgrade, lock this
 * way, so no order of removal is safe. */
#define HAMN_CONTROL_STOP_UNCERTAIN 5
int hamn_control_purge(const char *profile, int *absent);
/* vm up: start the VM of the workspace directory at path, creating its
 * ephemeral profile first when it has none (core/workspace.h names the
 * profile and describes its record). path is a borrowed NUL-terminated
 * absolute path; a zero resource keeps the profile's value, as for
 * hamn_control_start; git_mount is 1 to share the workspace's common git
 * directory and 0 not to.
 *
 * A new profile has the default settings and the requested resources,
 * except that the home directory is not shared: the workspace is shared
 * writable at its own path, then its common git directory, when git_mount
 * is set and it can be found, read-only at its own path. One that cannot
 * be found is a warning, not a failure. A profile that has the record of
 * this workspace is started as hamn_control_start starts it, with the
 * mounts it was created with.
 *
 * Returns the values of hamn_control_start, and
 *   2  path is not an absolute path to a directory of this user, or a
 *      profile cannot store its path: nothing under ~/.hamn was created;
 *   4  the profile exists without the record of this workspace, it has the
 *      record of another directory (also one that was at the same path
 *      before), or git_mount is 0 and it shares a git directory: nothing
 *      was changed;
 *   130  also when the call was cancelled while it waited for the lifecycle
 *      lock: it gave up before any change once it held the lock.
 * On HAMN_CONTROL_ADMISSION_REFUSED *result owns the refusal, as for
 * hamn_control_start. After every other return that found the workspace
 * *result owns {"profile","workspace"}: the profile's name and the
 * workspace's path. Release it with hamn_control_free; it is NULL
 * otherwise. When this call created the profile and its start does not
 * return 0, or its configuration write was not confirmed (1), the call
 * deletes the profile again (hamn_control_purge) and log_last_error() says
 * so after the reason: a refused or failed vm up leaves no profile
 * directory it made. Only its lock files stay, as after a purge.
 *
 * Holds the profile's lifecycle lock, waiting for it, from before it reads
 * the profile until its start, and that deletion, end. Before it waits for
 * that lock, and once the arguments are accepted, it purges the ephemeral
 * profiles of other workspaces that are gone (workspace_sweep in
 * core/workspace.h), whatever it then returns; "nothing was changed" above
 * speaks of this workspace's profile. */
int hamn_control_up(const char *path, unsigned cpus, unsigned memory_gib,
                    unsigned disk_gib, int git_mount, char **result);
/* vm down: stop the VM of the workspace directory at path when purge is 0,
 * as hamn_control_stop; purge it when purge is 1, as hamn_control_purge,
 * including the removal of what interrupted purges left. Only a profile with
 * the record of this workspace is stopped or purged. path is as for
 * hamn_control_up; absent and result are written on every return.
 *
 * Returns the values of hamn_control_stop or hamn_control_purge, and 2 and
 * 4 as hamn_control_up, after which nothing was changed. When the
 * workspace has no profile, a purge returns 0 with *absent 1 and creates,
 * locks and changes nothing, and a stop returns 1. *result owns
 * {"profile","workspace"} as for hamn_control_up once the workspace was
 * found, and is NULL otherwise. */
int hamn_control_down(const char *path, int purge, int *absent,
                      char **result);
/* Replace the VM disk of an existing profile: stop its VM, then delete
 * disk.img and what belongs to that disk (its EFI variables, the marker of
 * the guest deployment it received and the record of its damage). The next
 * start creates a new disk from the signed guest image. Everything inside
 * the VM is lost; config.yaml, the SSH key, the MAC address, the seed and
 * the logs stay. A profile without a disk is reset all the same.
 *
 * Returns 0, 2 for an invalid name, and 1 for any other failure, with the
 * reason in log_last_error(). The disk is deleted only after the VM is
 * known to be stopped: when vm stop fails the operation record says
 * outcomeUnknown and nothing is deleted. A removal that fails leaves the
 * files after it, and a retry continues. A completed reset also settles an
 * earlier operation whose outcome was unknown: no guest state of it is left.
 * Takes the lifecycle lock, then the mutation lock. */
int hamn_control_reset(const char *profile);
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
