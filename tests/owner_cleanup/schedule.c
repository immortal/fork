/* GNU/Linux cleanup oracle: suppress stale delivery, never signal a released ID. */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static pid_t owner, subject;
static const char *mode, *release, *pin_log;
static int held, stolen, idle_waits, depth, session;
static int (*real_waitid)(idtype_t, id_t, siginfo_t *, int);
__attribute__((constructor)) static void init(void) {
    owner = getpid();
    mode = getenv("FORK_CLEANUP_MODE");
    release = getenv("FORK_CLEANUP_RELEASE");
    pin_log = getenv("FORK_PIN_LOG");
    real_waitid = dlsym(RTLD_NEXT, "waitid");
}
static int named(const char *name) { return mode && strcmp(mode, name) == 0; }
static int owned(pid_t pid) {
    siginfo_t info = {0};
    return real_waitid(P_PID, pid, &info, WEXITED | WNOHANG | WNOWAIT) == 0;
}
static void delay(long nanoseconds) {
    struct timespec time = {0, nanoseconds};
    while (nanosleep(&time, &time) < 0 && errno == EINTR) {}
}
pid_t fork(void) {
    pid_t (*real)(void) = dlsym(RTLD_NEXT, "fork");
    pid_t pid = real();
    if (getpid() == owner && pid > 0) subject = pid;
    if (pid == 0) ++depth;
    return pid;
}
ssize_t read(int fd, void *buffer, size_t length) {
    ssize_t (*real)(int, void *, size_t) = dlsym(RTLD_NEXT, "read");
    if (getpid() != owner && (named("continue-handler") || named("continue-denied") || named("waitid-guard"))) {
        /* The old cleanup reaches a second idle wait without CONT. The new
         * cleanup must deliver (or safely skip) its first CONT before release. */
        for (int n = 0; n < 3000 && release && access(release, F_OK) != 0; ++n) delay(1000000);
    }
    return real(fd, buffer, length);
}
pid_t waitpid(pid_t pid, int *status, int flags) {
    pid_t (*real)(pid_t, int *, int) = dlsym(RTLD_NEXT, "waitpid");
    if (getpid() == owner && named("waitid-guard") && flags == WNOHANG) {
        /* Force exit into the gap before fallback observation, without consuming
         * it anywhere else. The helper is released by the preceding idle wait. */
        pid_t result = real(pid, status, 0);
        fprintf(stderr, "CONSUMED exact helper exit in fallback\n");
        return result;
    }
    pid_t result = real(pid, status, flags);
    if (getpid() == owner && named("waitid-guard") && result == 0 && release) {
        int fd = open(release, O_WRONLY | O_CREAT | O_CLOEXEC, 0600);
        if (fd >= 0) close(fd);
    }
    if (getpid() == owner && result == 0 && release
        && (named("continue-handler") || named("continue-denied")) && ++idle_waits == 2) {
        int fd = open(release, O_WRONLY | O_CREAT | O_CLOEXEC, 0600);
        if (fd >= 0) close(fd);
        fprintf(stderr, "RELEASED helper after second idle wait\n");
    }
    return result;
}
int execve(const char *path, char *const argv[], char *const envp[]) {
    int (*real)(const char *, char *const[], char *const[]) = dlsym(RTLD_NEXT, "execve");
    if (getpid() != owner && mode && strncmp(mode, "spawn-", 6) == 0)
        syscall(SYS_kill, getpid(), SIGSTOP);
    return real(path, argv, envp);
}
int sigaction(int sig, const struct sigaction *action, struct sigaction *previous) {
    int (*real)(int, const struct sigaction *, struct sigaction *) = dlsym(RTLD_NEXT, "sigaction");
    int result = real(sig, action, previous);
    if (result == 0 && getpid() == owner && sig == SIGCHLD && !action
        && named("guard-deadline") && held && !stolen) {
        stolen = 1;
        if (owned(subject)) {
            syscall(SYS_kill, subject, SIGCONT);
            int status;
            pid_t (*collect)(pid_t, int *, int) = dlsym(RTLD_NEXT, "waitpid");
            while (collect(subject, &status, 0) < 0 && errno == EINTR) {}
            fprintf(stderr, "STOLE helper after last idle wait, before KILL ownership check\n");
        }
    }
    return result;
}
int waitid(idtype_t type, id_t id, siginfo_t *info, int flags) {
    if (mode && strncmp(mode, "waitid-", 7) == 0) {
        /* The driver repeats each schedule per denial errno (default EPERM). */
        const char *denial = getenv("FORK_WAITID_ERRNO");
        errno = denial ? atoi(denial) : EPERM;
        return -1;
    }
    return real_waitid(type, id, info, flags);
}
pid_t setsid(void) {
    pid_t result = syscall(SYS_setsid);
    if (result > 0) session = 1;
    return result;
}
void _exit(int code) {
    if (named("waitid-failure") && session && depth == 1 && code == 0) code = 73;
    syscall(SYS_exit_group, code);
    __builtin_unreachable();
}
int kill(pid_t pid, int sig) {
    if (getpid() == owner && mode && strncmp(mode, "waitid-", 7) == 0
        && sig != 0 && (pid < -1 || pid > 0) && !owned(pid < 0 ? -pid : pid)) {
        fprintf(stderr, "UNSAFE unpinned waitid fallback delivery; suppressed\n");
        errno = ESRCH;
        return -1;
    }
    if (getpid() != owner && mode && strncmp(mode, "reaper-", 7) == 0
        && sig != 0 && (pid < -1 || (pid > 0 && sig == SIGKILL))) {
        int pinned = owned(pid < 0 ? -pid : pid);
        int fd = open(pin_log, O_WRONLY | O_APPEND | O_CREAT | O_CLOEXEC, 0600);
        if (fd >= 0) {
            dprintf(fd, "%s helper delivery target=%d signal=%d\n", pinned ? "PINNED" : "UNSAFE", pid, sig);
            close(fd);
        }
        if (!pinned) { errno = ESRCH; return -1; }
    }
    if (getpid() != owner || pid <= 0 || (sig != SIGCONT && sig != SIGKILL))
        return syscall(SYS_kill, pid, sig);
    if (!owned(pid)) {
        fprintf(stderr, "UNSAFE stale %s to pid=%d; suppressed\n", sig == SIGCONT ? "CONT" : "KILL", pid);
        errno = ESRCH;
        return -1;
    }
    if (named("continue-denied") && sig == SIGCONT) {
        fprintf(stderr, "DENIED helper resume while it can still finish via the socket\n");
        errno = EPERM;
        return -1;
    }
    if (named("guard-deadline") && sig == SIGCONT && !stolen) {
        held = 1;
        return 0; /* Keep the still-owned stopped helper until the deadline. */
    }
    struct sigaction action;
    sigaction(SIGCHLD, NULL, &action);
    if (action.sa_handler == SIG_IGN || (action.sa_flags & SA_NOCLDWAIT)) {
        /* Both subjects are stopped and still owned at this point. Let exit
         * win the production last-observation/delivery gap under auto-reaping. */
        syscall(SYS_kill, pid, sig == SIGCONT ? SIGKILL : SIGCONT);
        for (int n = 0; n < 2000 && owned(pid); ++n) delay(1000000);
        fprintf(stderr, "UNSAFE auto-reaping %s after exit, owned=%d; suppressed\n", sig == SIGCONT ? "CONT" : "KILL", owned(pid));
        errno = ESRCH;
        return -1;
    }
    if (!owned(pid)) {
        fprintf(stderr, "UNSAFE stale delivery after policy query; suppressed\n");
        errno = ESRCH;
        return -1;
    }
    return syscall(SYS_kill, pid, sig);
}
