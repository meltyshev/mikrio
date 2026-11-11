#ifndef MIKRIO_TEST_PLATFORM_H
#define MIKRIO_TEST_PLATFORM_H

#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#ifdef _WIN32
#define WIN32_LEAN_AND_MEAN
#include <process.h>
#include <wchar.h>
#include <windows.h>

typedef HANDLE test_thread;
typedef SRWLOCK test_mutex;
typedef CONDITION_VARIABLE test_condition;
#define TEST_MUTEX_INITIALIZER SRWLOCK_INIT

struct test_thread_entry {
  void *(*function)(void *);
  void *argument;
};

static unsigned __stdcall test_thread_entry(void *opaque) {
  struct test_thread_entry entry = *(struct test_thread_entry *)opaque;
  free(opaque);
  entry.function(entry.argument);
  return 0;
}

static inline int test_thread_start(test_thread *thread,
                                    void *(*function)(void *), void *argument) {
  struct test_thread_entry *entry = malloc(sizeof(*entry));
  assert(entry != NULL);
  *entry = (struct test_thread_entry){function, argument};
  *thread = (HANDLE)_beginthreadex(NULL, 0, test_thread_entry, entry, 0, NULL);

  if (*thread == NULL) {
    free(entry);
    return -1;
  }

  return 0;
}

static inline int test_thread_join(test_thread thread) {
  if (WaitForSingleObject(thread, INFINITE) != WAIT_OBJECT_0) {
    return -1;
  }

  return CloseHandle(thread) ? 0 : -1;
}

static inline int test_mutex_lock(test_mutex *mutex) {
  AcquireSRWLockExclusive(mutex);
  return 0;
}

static inline int test_mutex_unlock(test_mutex *mutex) {
  ReleaseSRWLockExclusive(mutex);
  return 0;
}

static inline void test_condition_init(test_condition *condition) {
  InitializeConditionVariable(condition);
}

static inline int test_condition_wait(test_condition *condition,
                                      test_mutex *mutex) {
  return SleepConditionVariableSRW(condition, mutex, INFINITE, 0) ? 0 : -1;
}

static inline int test_condition_broadcast(test_condition *condition) {
  WakeAllConditionVariable(condition);
  return 0;
}

static inline uint64_t test_monotonic_ms(void) { return GetTickCount64(); }

static inline void test_sleep_ms(uint32_t delay) { Sleep(delay); }

static inline int test_condition_wait_until(test_condition *condition,
                                            test_mutex *mutex,
                                            uint64_t deadline) {
  uint64_t now = test_monotonic_ms();
  assert(deadline > now && deadline - now < INFINITE);

  return SleepConditionVariableSRW(condition, mutex, (DWORD)(deadline - now), 0)
             ? 0
             : -1;
}

static inline void test_synchronization_destroy(test_condition *condition,
                                                test_mutex *mutex) {
  /* SRW locks and condition variables have no native destructor. Callers have
   * already joined all participants before this lifetime ends. */
  (void)condition;
  (void)mutex;
}

static inline uint8_t *test_guard_pages(size_t *page) {
  SYSTEM_INFO info;
  GetSystemInfo(&info);
  *page = info.dwPageSize;

  uint8_t *mapping =
      VirtualAlloc(NULL, 2 * *page, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);

  assert(mapping != NULL);
  DWORD previous;
  assert(VirtualProtect(mapping + *page, *page, PAGE_NOACCESS, &previous));
  return mapping;
}

static inline void test_guard_pages_release(uint8_t *mapping, size_t page) {
  (void)page;
  assert(VirtualFree(mapping, 0, MEM_RELEASE));
}

static inline void test_truncate_payload(const char *directory) {
  wchar_t root[4096], pattern[4096], path[4096], selected[4096];

  assert(MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, directory, -1, root,
                             4096) > 0);

  int length = swprintf(pattern, 4096, L"%ls\\objects\\*", root);
  assert(length > 0 && length < 4096);
  WIN32_FIND_DATAW shard;
  HANDLE shards = FindFirstFileW(pattern, &shard);
  assert(shards != INVALID_HANDLE_VALUE);
  size_t count = 0;

  do {
    if (shard.cFileName[0] == L'.') {
      continue;
    }

    assert(shard.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY);

    length =
        swprintf(pattern, 4096, L"%ls\\objects\\%ls\\*", root, shard.cFileName);

    assert(length > 0 && length < 4096);
    WIN32_FIND_DATAW file;
    HANDLE files = FindFirstFileW(pattern, &file);
    assert(files != INVALID_HANDLE_VALUE);

    do {
      if (file.cFileName[0] == L'.') {
        continue;
      }

      assert(!(file.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY));

      length = swprintf(path, 4096, L"%ls\\objects\\%ls\\%ls", root,
                        shard.cFileName, file.cFileName);

      assert(length > 0 && length < 4096);
      assert(++count == 1);
      memcpy(selected, path, ((size_t)length + 1) * sizeof(wchar_t));
    } while (FindNextFileW(files, &file));

    assert(GetLastError() == ERROR_NO_MORE_FILES);
    assert(FindClose(files));
  } while (FindNextFileW(shards, &shard));

  assert(GetLastError() == ERROR_NO_MORE_FILES);
  assert(FindClose(shards));
  assert(count == 1);
  FILE *damaged = _wfopen(selected, L"wb");
  assert(damaged != NULL && fclose(damaged) == 0);
}

#else
#include <glob.h>
#include <pthread.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>

typedef pthread_t test_thread;
typedef pthread_mutex_t test_mutex;
typedef pthread_cond_t test_condition;
#define TEST_MUTEX_INITIALIZER PTHREAD_MUTEX_INITIALIZER

static inline int test_thread_start(test_thread *thread,
                                    void *(*function)(void *), void *argument) {
  return pthread_create(thread, NULL, function, argument);
}

static inline int test_thread_join(test_thread thread) {
  return pthread_join(thread, NULL);
}

static inline int test_mutex_lock(test_mutex *mutex) {
  return pthread_mutex_lock(mutex);
}

static inline int test_mutex_unlock(test_mutex *mutex) {
  return pthread_mutex_unlock(mutex);
}

static inline void test_condition_init(test_condition *condition) {
  pthread_condattr_t attributes;
  assert(pthread_condattr_init(&attributes) == 0);
#ifndef __APPLE__
  assert(pthread_condattr_setclock(&attributes, CLOCK_MONOTONIC) == 0);
#endif
  assert(pthread_cond_init(condition, &attributes) == 0);
  assert(pthread_condattr_destroy(&attributes) == 0);
}

static inline int test_condition_wait(test_condition *condition,
                                      test_mutex *mutex) {
  return pthread_cond_wait(condition, mutex);
}

static inline int test_condition_broadcast(test_condition *condition) {
  return pthread_cond_broadcast(condition);
}

static inline uint64_t test_monotonic_ms(void) {
  struct timespec now;
  assert(clock_gettime(CLOCK_MONOTONIC, &now) == 0);
  return (uint64_t)now.tv_sec * 1000 + (uint64_t)now.tv_nsec / 1000000;
}

static inline void test_sleep_ms(uint32_t delay) {
  struct timespec interval = {(time_t)(delay / 1000),
                              (long)(delay % 1000) * 1000000L};

  assert(nanosleep(&interval, NULL) == 0);
}

static inline int test_condition_wait_until(test_condition *condition,
                                            test_mutex *mutex,
                                            uint64_t deadline) {
#ifdef __APPLE__
  uint64_t now = test_monotonic_ms();
  assert(deadline > now);
  uint64_t remaining = deadline - now;

  struct timespec limit = {(time_t)(remaining / 1000),
                           (long)(remaining % 1000) * 1000000L};

  return pthread_cond_timedwait_relative_np(condition, mutex, &limit);
#else
  struct timespec limit = {(time_t)(deadline / 1000),
                           (long)(deadline % 1000) * 1000000L};

  return pthread_cond_timedwait(condition, mutex, &limit);
#endif
}

static inline void test_synchronization_destroy(test_condition *condition,
                                                test_mutex *mutex) {
  assert(pthread_cond_destroy(condition) == 0);
  assert(pthread_mutex_destroy(mutex) == 0);
}

static inline uint8_t *test_guard_pages(size_t *page) {
  long size = sysconf(_SC_PAGESIZE);
  assert(size > 0);
  *page = (size_t)size;

  uint8_t *mapping = mmap(NULL, 2 * *page, PROT_READ | PROT_WRITE,
                          MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);

  assert(mapping != MAP_FAILED);
  assert(mprotect(mapping + *page, *page, PROT_NONE) == 0);
  return mapping;
}

static inline void test_guard_pages_release(uint8_t *mapping, size_t page) {
  assert(munmap(mapping, 2 * page) == 0);
}

static inline void test_truncate_payload(const char *directory) {
  char pattern[4096];
  int length = snprintf(pattern, sizeof(pattern), "%s/objects/*/*", directory);
  assert(length > 0 && (size_t)length < sizeof(pattern));
  glob_t matches = {0};
  assert(glob(pattern, 0, NULL, &matches) == 0 && matches.gl_pathc == 1);
  FILE *damaged = fopen(matches.gl_pathv[0], "wb");
  assert(damaged != NULL && fclose(damaged) == 0);
  globfree(&matches);
}
#endif
#endif
