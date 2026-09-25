#define _GNU_SOURCE
#include <crypt.h>
#include <stdio.h>
#include <string.h>

/* Passwords travel only over stdin. crypt_gensalt uses the OS random source.
 * Supplying an existing hash retains its algorithm, salt and rounds. */
int main(int argc, char **argv) {
    char password[256], salt[CRYPT_GENSALT_OUTPUT_SIZE];
    struct crypt_data data = {0};
    if (argc > 2 || !fgets(password, sizeof password, stdin)) return 1;
    size_t length = strlen(password);
    if (!length || password[length - 1] != '\n') return 1;
    password[--length] = 0;
    if (!length) return 1;
    const char *setting = argc == 2 ? argv[1] :
        crypt_gensalt_rn("$6$", 0, NULL, 0, salt, sizeof salt);
    char *hash = setting ? crypt_r(password, setting, &data) : NULL;
    explicit_bzero(password, sizeof password);
    if (!hash || hash[0] == '*') return 1;
    int result = puts(hash) < 0;
    explicit_bzero(&data, sizeof data);
    return result;
}
