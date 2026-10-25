/* SPDX-License-Identifier: Apache-2.0
 * Linux test interposer: fail only fsync on the exact owned fixture path.
 * All other calls use the real kernel syscall. Never linked into Ojo products.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdlib.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

int fsync(int fd) {
    const char *target = getenv("OJO_SYNC_FAULT_TARGET");
    const char *flag = getenv("OJO_SYNC_FAULT_FLAG");
    const char *counter = getenv("OJO_SYNC_FAULT_COUNTER");
    struct stat wanted, actual;
    int matching = 0;
    if (target && flag && counter) {
        /* Atomic Event creation retains an fd opened through its temporary
         * name. Match the final owned inode, including that original fd. */
        matching = stat(target, &wanted) == 0 && fstat(fd, &actual) == 0
            && wanted.st_dev == actual.st_dev && wanted.st_ino == actual.st_ino;
    }
    if (matching) {
        int fail = access(flag, F_OK) == 0;
        int log = open(counter, O_WRONLY | O_CREAT | O_APPEND | O_CLOEXEC, 0600);
        char event = fail ? 'F' : 'S';
        if (log < 0 || write(log, &event, 1) != 1 || close(log) != 0) _exit(125);
        if (fail) { errno = EIO; return -1; }
    }
    return (int)syscall(SYS_fsync, fd);
}
