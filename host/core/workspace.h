#ifndef HAMN_WORKSPACE_H
#define HAMN_WORKSPACE_H

/*
 * A workspace is a directory of this user, such as a git worktree, that one
 * ephemeral profile serves: vm up creates or reuses it, vm down stops or
 * purges it.
 *
 * Identity. The directory is opened, and its path is what fcntl(F_GETPATH)
 * returns for it: absolute, without symbolic links, and with each name in
 * the case it has on disk, so that two spellings of one directory on a
 * case-insensitive volume (.../Hamn and .../hamn) give one path. Its
 * (st_dev, st_ino) tell it apart from a directory made later at the same
 * path.
 *
 * Profile name. "ws-" and the first 12 lowercase hexadecimal digits of the
 * SHA-256 of the path's bytes.
 *
 * Record. ~/.hamn/<profile>/ephemeral, mode 0600, holds one line: st_dev
 * and st_ino in decimal, then the path, separated by single spaces and ended
 * by a newline, as in "16777232 366033121 /Users/me/project\n". Only a
 * profile with this record is ephemeral; vm up and vm down refuse a profile
 * of the same name without it, or whose record names another directory.
 */

#include <limits.h>
#include <sys/types.h>

#include "core/profile.h"

#define WORKSPACE_RECORD_FILE "ephemeral"
#define WORKSPACE_REASON_CAP (2 * PATH_MAX + 128)

struct workspace {
    char path[PATH_MAX]; /* as F_GETPATH gives it */
    dev_t device;
    ino_t inode;
    char profile[PROFILE_NAME_CAP];
};

/* Identifies the directory at path, which must be absolute. Returns 0 with
 * *workspace filled, or -1 having logged why: the path is not absolute,
 * does not exist, is not a directory, is not this user's, or cannot be
 * opened. Reads only. */
int workspace_identify(const char *path, struct workspace *workspace);

/* The common git directory of the workspace directory at path, found the
 * way git finds it from the workspace's .git, without running git, when
 * that directory is the workspace's own:
 *   - .git is a directory: that directory;
 *   - .git is a file "gitdir: <dir>" (a linked worktree): <dir>, relative to
 *     the workspace unless absolute; then the directory named by
 *     <dir>/commondir, relative to <dir> unless absolute, when that file
 *     exists, and <dir> itself when it does not.
 * Trailing line breaks of these files are ignored, as git ignores them, and
 * symbolic links below .git are followed. As git, it accepts the pair only
 * when the git directory holds a HEAD file and the common one objects and
 * refs directories, so that a .git file naming any other directory, such as
 * the home directory, shares nothing. A .git file is input from the
 * workspace, so its git directory must also be one that git made for this
 * workspace: <common>/worktrees/<name>, whose gitdir file names the
 * workspace's .git (relative to <dir> unless absolute). A submodule's git
 * directory, another repository's, and a symbolic link .git share nothing.
 * Returns 0 with git holding the common directory's path as F_GETPATH gives
 * it, or -1 with git empty and reason saying why there is none: no .git, a
 * symbolic link or a .git of another kind, a file in another form, a file
 * larger than PATH_MAX bytes, a directory that cannot be opened, one that
 * is not laid out as a repository, or a git directory of another worktree,
 * of a submodule or of another repository. Reads only, and logs nothing. */
int workspace_git_directory(const char *path, char git[PATH_MAX],
                            char reason[WORKSPACE_REASON_CAP]);

/* The configuration of the profile that vm up creates for workspace: the
 * defaults of every setting, except that the home directory is not shared;
 * the workspace is mounted writable at its own path, then git, unless
 * empty, read-only at its own path. Sets the name; dir is left empty. */
void workspace_profile(const struct workspace *workspace, const char *git,
                       struct profile *profile);

/* Writes the record of workspace into the directory of profile, atomically.
 * Returns 0, or -1 with errno. */
int workspace_record_write(const struct profile *profile,
                           const struct workspace *workspace);

/* Reads the record in the directory of profile into *recorded, with the
 * profile name derived from its path. Returns 1, 0 when there is none (no
 * file, or no profile directory), or -1 having logged why for a record that
 * is not a regular file of this user or not in the form above. */
int workspace_record_read(const struct profile *profile,
                          struct workspace *recorded);

/* Whether a and b name the same directory: equal paths, devices and
 * inodes. */
int workspace_same(const struct workspace *a, const struct workspace *b);

/* The record of workspace as above, NUL-terminated, into text. Returns its
 * length, or -1 with errno ENAMETOOLONG. */
#define WORKSPACE_RECORD_CAP (PATH_MAX + 64)
int workspace_record_format(const struct workspace *workspace,
                            char text[WORKSPACE_RECORD_CAP]);

/* Parses the NUL-terminated text as a record, newline included, into
 * *recorded with the profile name derived from its path. Returns 0, or -1
 * for text not in the form above. Logs nothing. */
int workspace_record_parse(const char *text, struct workspace *recorded);

/*
 * Reclaim. A workspace's VM is reclaimed, stopped and purged, once nothing
 * is at the workspace's recorded path any longer: a git worktree that its
 * tool deleted. A directory found at that path with another device or inode
 * never reclaims it, because attaching a volume again can change the device
 * of the same directory; vm up refuses such a profile instead. A profile
 * without a record is never reclaimed, and nothing is decided by a PID or a
 * name alone: a purge needs the VM known to be stopped (vm_process_probe).
 *
 * Two places reclaim without being asked:
 *   - vmrun, while the VM runs, checks the path every 10 seconds. The first
 *     check that finds it absent asks the guest to stop, as SIGTERM does,
 *     and once vmrun has removed its process identity at exit it starts
 *     "hamn workspace-reclaim <record>" detached, which purges the profile
 *     (cmd_workspace_reclaim in cli.h). vmrun never deletes its own
 *     directory.
 *   - workspace_sweep, from vm list and vm up, for a VM that stopped
 *     without vmrun noticing, or whose reclaim failed.
 */

/* What lstat says about path: PRESENT when something is there, also a file
 * or a symbolic link; ABSENT when it fails with ENOENT or ENOTDIR; UNKNOWN
 * for any other failure, such as EACCES, which never reclaims. */
enum workspace_presence {
    WORKSPACE_PRESENT,
    WORKSPACE_ABSENT,
    WORKSPACE_UNKNOWN,
};
enum workspace_presence workspace_presence(const char *path);

/* Purges every profile of ~/.hamn whose record names a path that is
 * absent, while its VM is known to be stopped. A profile whose lifecycle
 * lock another process holds is skipped, never waited for, and no lock file
 * is made for a profile that is not such a candidate. Never fails its
 * caller and leaves no recorded reason (log_last_error() is empty after
 * it); what it purges and what it cannot purge it prints. Call it holding
 * no profile lock. Implemented with the control calls, in
 * cmd/cmd_workspace.c. */
void workspace_sweep(void);

#endif
