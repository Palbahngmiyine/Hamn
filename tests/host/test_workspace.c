/* core/workspace.c: which directory a workspace path names, the profile name
 * and record that follow from it, the common git directory a VM shares, and
 * the profile vm up creates. Expected names come from SHA-256 values
 * computed apart from Hamn (shasum -a 256), and expected git directories
 * from the layouts git itself writes: a linked worktree's .git file, its
 * commondir, and a plain repository's .git directory. No VM is involved;
 * the vm-up suite covers the control calls. */
#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

#include "core/log.h"
#include "core/profile.h"
#include "core/workspace.h"

static char root[PATH_MAX]; /* the real path of this test's directory */

static const char *at(const char *name)
{
    static char path[2][PATH_MAX];
    static int next;
    next = !next;
    snprintf(path[next], sizeof(path[next]), "%s/%s", root, name);
    return path[next];
}

static void make_directory(const char *name)
{
    assert(mkdir(at(name), 0700) == 0);
}

static void write_file(const char *path, const char *text)
{
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0600);
    size_t length = strlen(text);
    assert(fd >= 0 && write(fd, text, length) == (ssize_t)length &&
           close(fd) == 0);
}

static int logged(const char *text)
{
    return strstr(log_last_error(), text) != NULL;
}

static void two_spellings_of_one_directory_are_one_workspace(void)
{
    make_directory("Tree");
    assert(symlink(at("Tree"), at("link")) == 0);
    struct workspace expected;
    assert(workspace_identify(at("Tree"), &expected) == 0);
    assert(strcmp(expected.path, at("Tree")) == 0);
    struct stat status;
    assert(stat(at("Tree"), &status) == 0);
    assert(expected.device == status.st_dev && expected.inode == status.st_ino);
    assert(strncmp(expected.profile, "ws-", 3) == 0 &&
           strlen(expected.profile) == 15);
    /* The volume of /tmp is case-insensitive, as macOS installs it. */
    assert(access(at("tree"), F_OK) == 0);
    for (const char *const *name = (const char *const[]){ "tree", "TREE",
             "link", "Tree/.", "Tree/../Tree", "link/", NULL };
         *name; name++) {
        struct workspace other;
        assert(workspace_identify(at(*name), &other) == 0);
        assert(workspace_same(&other, &expected));
        assert(strcmp(other.profile, expected.profile) == 0);
    }
    make_directory("Other");
    struct workspace other;
    assert(workspace_identify(at("Other"), &other) == 0);
    assert(!workspace_same(&other, &expected) &&
           strcmp(other.profile, expected.profile) != 0);
}

static void a_missing_file_relative_or_foreign_path_is_refused(void)
{
    struct workspace workspace;
    assert(workspace_identify(at("missing"), &workspace) == -1);
    assert(logged("does not exist"));
    write_file(at("file"), "x\n");
    assert(workspace_identify(at("file"), &workspace) == -1);
    assert(logged("is not a directory"));
    assert(workspace_identify(at("file/below"), &workspace) == -1);
    assert(logged("is not a directory"));
    assert(workspace_identify("relative/path", &workspace) == -1);
    assert(logged("must be an absolute path"));
    assert(workspace_identify(NULL, &workspace) == -1);
    if (geteuid() != 0) {
        assert(workspace_identify("/usr", &workspace) == -1);
        assert(logged("workspace /usr is not owned by this user"));
    }
    assert(workspace.profile[0] == '\0');
}

/* git_directory of directory name: 0 with git equal to the real path of
 * expected, or -1 with a reason that contains expected. */
static void git_case(const char *name, int rc, const char *expected)
{
    char git[PATH_MAX], reason[WORKSPACE_REASON_CAP];
    assert(workspace_git_directory(at(name), git, reason) == rc);
    if (rc == 0) {
        char real[PATH_MAX];
        assert(realpath(at(expected), real));
        if (strcmp(git, real) != 0) {
            fprintf(stderr, "%s: %s, not %s\n", name, git, real);
            abort();
        }
    } else {
        assert(git[0] == '\0');
        if (!strstr(reason, expected)) {
            fprintf(stderr, "%s: %s\n", name, reason);
            abort();
        }
    }
}

/* A main repository at main with a linked worktree whose git directory is
 * main/.git/worktrees/<name>, as `git worktree add` lays it out. */
static void linked(const char *name, const char *gitdir_line,
                   const char *commondir)
{
    char path[PATH_MAX];
    snprintf(path, sizeof(path), "main/.git/worktrees/%s", name);
    make_directory(path);
    snprintf(path, sizeof(path), "%s/main/.git/worktrees/%s/HEAD", root, name);
    write_file(path, "0123456789abcdef0123456789abcdef01234567\n");
    if (commondir) {
        snprintf(path, sizeof(path), "%s/main/.git/worktrees/%s/commondir",
                 root, name);
        write_file(path, commondir);
    }
    make_directory(name);
    snprintf(path, sizeof(path), "%s/%s/.git", root, name);
    write_file(path, gitdir_line);
}

/* The directory name laid out as a repository's common git directory. */
static void repository(const char *name)
{
    char path[PATH_MAX];
    make_directory(name);
    for (const char *const *entry = (const char *const[]){ "objects", "refs",
             NULL }; *entry; entry++) {
        snprintf(path, sizeof(path), "%s/%s", name, *entry);
        make_directory(path);
    }
    snprintf(path, sizeof(path), "%s/%s/HEAD", root, name);
    write_file(path, "ref: refs/heads/main\n");
}

static void the_common_git_directory_is_found_as_git_finds_it(void)
{
    make_directory("main");
    repository("main/.git");
    make_directory("main/.git/worktrees");
    git_case("main", 0, "main/.git");

    char line[PATH_MAX + 32];
    snprintf(line, sizeof(line), "gitdir: %s\n", at("main/.git/worktrees/wt"));
    linked("wt", line, "../..\n");
    git_case("wt", 0, "main/.git");

    linked("relative", "gitdir: ../main/.git/worktrees/relative", "../..");
    git_case("relative", 0, "main/.git");

    snprintf(line, sizeof(line), "%s\r\n", at("main/.git"));
    linked("crlf", "gitdir: ../main/.git/worktrees/crlf\r\n", line);
    git_case("crlf", 0, "main/.git");

    /* A submodule's git directory has no commondir: it is its own. */
    linked("module", "gitdir: ../main/.git/worktrees/module\n", NULL);
    make_directory("main/.git/worktrees/module/objects");
    make_directory("main/.git/worktrees/module/refs");
    git_case("module", 0, "main/.git/worktrees/module");

    make_directory("linked-git");
    assert(symlink(at("main/.git"), at("linked-git/.git")) == 0);
    git_case("linked-git", 0, "main/.git");

    make_directory("plain");
    git_case("plain", -1, "/.git: there is none");
    linked("prefix", "gitdir:../main/.git/worktrees/prefix\n", "../..\n");
    git_case("prefix", -1, "does not name a git directory");
    linked("empty", "gitdir: \n", "../..\n");
    git_case("empty", -1, "does not name a git directory");
    linked("gone", "gitdir: ../main/.git/worktrees/elsewhere\n", "../..\n");
    git_case("gone", -1, "cannot open the git directory "
             "../main/.git/worktrees/elsewhere");
    linked("lost", "gitdir: ../main/.git/worktrees/lost\n", "../../lost\n");
    git_case("lost", -1, "cannot open the common git directory ../../lost");
    /* A .git file can name any directory: the home directory shares nothing. */
    make_directory("home-like");
    make_directory("named-home");
    snprintf(line, sizeof(line), "gitdir: %s\n", at("home-like"));
    write_file(at("named-home/.git"), line);
    git_case("named-home", -1, "is not one: it lacks a HEAD file");
    linked("stray", "gitdir: ../main/.git/worktrees/stray\n", "..\n");
    git_case("stray", -1, "is not one: it lacks a HEAD file");
    make_directory("unborn");
    make_directory("unborn/.git");
    write_file(at("unborn/.git/HEAD"), "ref: refs/heads/main\n");
    git_case("unborn", -1, "is not one: it lacks a HEAD file");
    make_directory("fifo");
    assert(mkfifo(at("fifo/.git"), 0600) == 0);
    git_case("fifo", -1, "is neither a directory nor a file");

    make_directory("large");
    char text[PATH_MAX + 64];
    memset(text, 'a', sizeof(text) - 1);
    memcpy(text, "gitdir: ", 8);
    text[sizeof(text) - 1] = '\0';
    write_file(at("large/.git"), text);
    git_case("large", -1, "File too large");
}

static void a_new_profile_shares_the_workspace_and_its_git_directory(void)
{
    struct workspace workspace;
    assert(workspace_identify(at("wt"), &workspace) == 0);
    char git[PATH_MAX], reason[WORKSPACE_REASON_CAP];
    assert(workspace_git_directory(workspace.path, git, reason) == 0);
    struct profile profile;
    workspace_profile(&workspace, git, &profile);
    assert(strcmp(profile.name, workspace.profile) == 0 && !profile.dir[0]);
    assert(!profile.mount_home && profile.mount_count == 2);
    assert(strcmp(profile.mounts[0].location, workspace.path) == 0 &&
           strcmp(profile.mounts[0].mount_point, workspace.path) == 0 &&
           profile.mounts[0].writable);
    assert(strcmp(profile.mounts[1].location, git) == 0 &&
           strcmp(profile.mounts[1].mount_point, git) == 0 &&
           !profile.mounts[1].writable);
    struct profile defaults;
    profile_defaults(&defaults);
    defaults.mount_home = 0;
    defaults.mount_count = profile.mount_count;
    memcpy(defaults.mounts, profile.mounts, sizeof(defaults.mounts));
    assert(profile_diff(&profile, &defaults) == 0);
    assert(profile_storable(&profile));

    workspace_profile(&workspace, "", &profile);
    assert(profile.mount_count == 1 && profile.mounts[0].writable);
    assert(profile_storable(&profile));
}

/* A profile directory of HOME/.hamn for a record. */
static void record_profile(struct profile *profile, const char *name)
{
    memset(profile, 0, sizeof(*profile));
    snprintf(profile->name, sizeof(profile->name), "%s", name);
    assert(profile_locate(profile) == 0);
}

static void the_record_names_the_directory_and_nothing_else(void)
{
    char home[PATH_MAX];
    snprintf(home, sizeof(home), "%s", at("home"));
    make_directory("home");
    make_directory("home/.hamn");
    assert(setenv("HOME", home, 1) == 0);
    struct workspace workspace, recorded;
    assert(workspace_identify(at("wt"), &workspace) == 0);
    struct profile profile;
    record_profile(&profile, workspace.profile);
    assert(workspace_record_read(&profile, &recorded) == 0);
    assert(mkdir(profile.dir, 0700) == 0);
    assert(workspace_record_read(&profile, &recorded) == 0);

    assert(workspace_record_write(&profile, &workspace) == 0);
    char path[PATH_MAX], expected[PATH_MAX + 64], text[PATH_MAX + 64];
    snprintf(path, sizeof(path), "%s/ephemeral", profile.dir);
    struct stat status;
    assert(lstat(path, &status) == 0 && (status.st_mode & 07777) == 0600);
    assert(stat(at("wt"), &status) == 0);
    snprintf(expected, sizeof(expected), "%llu %llu %s\n",
             (unsigned long long)status.st_dev,
             (unsigned long long)status.st_ino, workspace.path);
    int fd = open(path, O_RDONLY);
    ssize_t length = read(fd, text, sizeof(text) - 1);
    assert(fd >= 0 && length > 0 && close(fd) == 0);
    text[length] = '\0';
    assert(strcmp(text, expected) == 0);
    assert(workspace_record_read(&profile, &recorded) == 1);
    assert(workspace_same(&recorded, &workspace) &&
           strcmp(recorded.profile, workspace.profile) == 0);

    /* The name follows from the recorded path alone. */
    write_file(path, "1 2 /Users/me/project\n");
    assert(workspace_record_read(&profile, &recorded) == 1);
    assert(strcmp(recorded.profile, "ws-8779b387e6b0") == 0);
    assert(recorded.device == 1 && recorded.inode == 2);
    assert(!workspace_same(&recorded, &workspace));
    write_file(path, "1 2 /Users/me/my project\n");
    assert(workspace_record_read(&profile, &recorded) == 1);
    assert(strcmp(recorded.profile, "ws-d713d602194c") == 0 &&
           strcmp(recorded.path, "/Users/me/my project") == 0);

    for (const char *const *bad = (const char *const[]){ "", "1 2 /p",
             "1 2 relative\n", "1 2 /p\n\n", "1 2 /a\nb\n", "x 2 /p\n",
             "1  2 /p\n", "+1 2 /p\n", "1 -2 /p\n", "1 2\n",
             "99999999999999999999 2 /p\n", "1 2  /p\n", NULL };
         *bad; bad++) {
        write_file(path, *bad);
        assert(workspace_record_read(&profile, &recorded) == -1);
        assert(logged("is not \"<device> <inode> <path>\" on one line"));
        assert(recorded.path[0] == '\0');
    }
    assert(unlink(path) == 0);
    write_file(at("elsewhere"), expected);
    assert(symlink(at("elsewhere"), path) == 0);
    assert(workspace_record_read(&profile, &recorded) == -1);
    assert(logged("cannot read the workspace record"));
    assert(unlink(path) == 0 && mkdir(path, 0700) == 0);
    assert(workspace_record_read(&profile, &recorded) == -1);
    assert(rmdir(path) == 0);
}

static void a_directory_made_again_at_the_same_path_is_another(void)
{
    make_directory("again");
    struct workspace before, after;
    assert(workspace_identify(at("again"), &before) == 0);
    /* Keep the old directory alive, so that its inode cannot be reused. */
    assert(rename(at("again"), at("again-old")) == 0);
    make_directory("again");
    assert(workspace_identify(at("again"), &after) == 0);
    assert(strcmp(before.path, after.path) == 0 &&
           strcmp(before.profile, after.profile) == 0);
    assert(!workspace_same(&before, &after));
}

int main(void)
{
    char temporary[] = "/tmp/hamn-workspace-XXXXXX";
    assert(mkdtemp(temporary) && realpath(temporary, root));
    pid_t runner = fork();
    assert(runner >= 0);
    if (runner == 0) {
        alarm(60);
        two_spellings_of_one_directory_are_one_workspace();
        a_missing_file_relative_or_foreign_path_is_refused();
        the_common_git_directory_is_found_as_git_finds_it();
        a_new_profile_shares_the_workspace_and_its_git_directory();
        the_record_names_the_directory_and_nothing_else();
        a_directory_made_again_at_the_same_path_is_another();
        _exit(0);
    }
    int status = 0;
    pid_t reaped;
    do {
        reaped = waitpid(runner, &status, 0);
    } while (reaped < 0 && errno == EINTR);
    assert(reaped == runner);
    char command[2 * PATH_MAX + 48];
    snprintf(command, sizeof(command), "/bin/rm -rf '%s'", root);
    int removed = system(command) == 0;
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        fprintf(stderr, "FAIL: workspace cases (wait status %d)\n", status);
        return 1;
    }
    if (!removed) {
        fprintf(stderr, "FAIL: cannot remove %s\n", root);
        return 1;
    }
    printf("workspace: all cases passed\n");
    return 0;
}
