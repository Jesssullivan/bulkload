// DYLD interposer: log every F_FULLFSYNC / F_BARRIERFSYNC / fsync / fdatasync
// with the fd's path and st_dev, so a run shows which device each flush hits.
// Build: cc -dynamiclib -o libinterpose.dylib interpose.c
// Use:   DYLD_INSERT_LIBRARIES=$PWD/libinterpose.dylib FLUSHLOG=/path/log bulkload-agent copy ...
#include <fcntl.h>
#include <stdarg.h>
#include <stdio.h>
#include <string.h>
#include <sys/param.h>
#include <sys/stat.h>
#include <unistd.h>
#include <stdlib.h>

#define DYLD_INTERPOSE(_replacement, _replacee) \
  __attribute__((used)) static struct { const void *replacement; const void *replacee; } \
  _interpose_##_replacee __attribute__((section("__DATA,__interpose"))) = \
  {(const void *)(unsigned long)&_replacement, (const void *)(unsigned long)&_replacee};

extern int fdatasync(int);

static void note(const char *what, int fd, int rc) {
  const char *log = getenv("FLUSHLOG");
  char path[MAXPATHLEN] = "?";
  struct stat st;
  long long dev = -1;
  fcntl(fd, F_GETPATH, path);
  if (fstat(fd, &st) == 0) dev = (long long)st.st_dev;
  char line[MAXPATHLEN + 128];
  int n = snprintf(line, sizeof line, "%s dev=%lld rc=%d %s\n", what, dev, rc, path);
  if (log) {
    int out = open(log, O_WRONLY | O_CREAT | O_APPEND, 0600);
    if (out >= 0) { write(out, line, n); close(out); }
  }
}

static int my_fcntl(int fd, int cmd, ...) {
  va_list ap;
  va_start(ap, cmd);
  void *arg = va_arg(ap, void *);
  va_end(ap);
  int rc = fcntl(fd, cmd, arg);
  if (cmd == F_FULLFSYNC) note("F_FULLFSYNC", fd, rc);
  else if (cmd == F_BARRIERFSYNC) note("F_BARRIERFSYNC", fd, rc);
  return rc;
}
static int my_fsync(int fd) { int rc = fsync(fd); note("fsync", fd, rc); return rc; }
static int my_fdatasync(int fd) { int rc = fdatasync(fd); note("fdatasync", fd, rc); return rc; }

DYLD_INTERPOSE(my_fcntl, fcntl)
DYLD_INTERPOSE(my_fsync, fsync)
DYLD_INTERPOSE(my_fdatasync, fdatasync)
