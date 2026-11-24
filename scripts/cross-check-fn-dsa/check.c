/*
 * Gives Stein's FN-DSA-512 test vectors to a second implementation: the C
 * code by the author of the `fn-dsa` crate. See ../cross-check-fn-dsa.sh.
 *
 * The vectors are lines of words, bytes in hex and "-" for no bytes:
 *
 *   key <index> <seed> <secret key> <public key>
 *   sig <key index> <seed> <message> <signature>
 *   ver <key index> <message> <signature> <1 if it verifies, else 0>
 *
 * For each line the C code must make the same key pair from the seed, make
 * the same signature from the seed, or reach the same verdict. This program
 * is a test. It is never built into a Stein binary.
 */

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "fndsa.h"

#define LOGN       FNDSA_LOGN_512
#define SECRET_LEN FNDSA_SIGN_KEY_SIZE(LOGN)
#define PUBLIC_LEN FNDSA_VRFY_KEY_SIZE(LOGN)
#define SIG_LEN    FNDSA_SIGNATURE_SIZE(LOGN)
#define MAX_KEYS   64
#define MAX_LINE   (1 << 16)
#define MAX_FIELD  (MAX_LINE / 2)
/* Room for the working buffers of key generation, signing and verifying
   (fndsa.h gives 11295, 30239 and 2079 bytes for this degree). */
#define TMP_LEN    (1 << 16)

static uint8_t secrets[MAX_KEYS][SECRET_LEN];
static uint8_t publics[MAX_KEYS][PUBLIC_LEN];
static int have[MAX_KEYS];
static char line[MAX_LINE];
static uint8_t field[4][MAX_FIELD];
static size_t field_len[4];
static uint8_t tmp[TMP_LEN];
static unsigned long line_number;

static void
stop(const char *what)
{
	fprintf(stderr, "cross-check: line %lu: %s\n", line_number, what);
	exit(2);
}

static int
nibble(int c)
{
	if (c >= '0' && c <= '9') {
		return c - '0';
	}
	if (c >= 'a' && c <= 'f') {
		return c - 'a' + 10;
	}
	return -1;
}

/* Decodes the next word of the line into field[n]. */
static void
bytes(int n)
{
	const char *w = strtok(NULL, " \r\n");
	size_t len, i;

	if (w == NULL) {
		stop("a word is missing");
	}
	if (strcmp(w, "-") == 0) {
		field_len[n] = 0;
		return;
	}
	len = strlen(w);
	if ((len & 1) != 0 || len / 2 > MAX_FIELD) {
		stop("not hex, or too long");
	}
	for (i = 0; i < len; i += 2) {
		int hi = nibble((unsigned char)w[i]);
		int lo = nibble((unsigned char)w[i + 1]);
		if (hi < 0 || lo < 0) {
			stop("not hex");
		}
		field[n][i / 2] = (uint8_t)((hi << 4) | lo);
	}
	field_len[n] = len / 2;
}

static unsigned long
number(void)
{
	const char *w = strtok(NULL, " \r\n");
	char *end;
	unsigned long v;

	if (w == NULL) {
		stop("a word is missing");
	}
	v = strtoul(w, &end, 10);
	if (*end != 0) {
		stop("not a number");
	}
	return v;
}

static unsigned long
key_index(void)
{
	unsigned long k = number();

	if (k >= MAX_KEYS) {
		stop("too many keys");
	}
	return k;
}

int
main(int argc, char *argv[])
{
	FILE *f;
	unsigned long count[3] = { 0, 0, 0 }, differ[3] = { 0, 0, 0 };
	static const char *const names[3] = { "keys", "signatures", "verdicts" };
	int i, failed = 0;

	if (argc != 2) {
		fprintf(stderr, "usage: check <vectors file>\n");
		return 2;
	}
	f = fopen(argv[1], "r");
	if (f == NULL) {
		perror(argv[1]);
		return 2;
	}
	while (fgets(line, sizeof line, f) != NULL) {
		const char *kind;
		unsigned long k;

		line_number++;
		if (strchr(line, '\n') == NULL) {
			stop("too long");
		}
		kind = strtok(line, " \r\n");
		if (kind == NULL) {
			continue;
		}
		if (strcmp(kind, "key") == 0) {
			uint8_t secret[SECRET_LEN], public[PUBLIC_LEN];

			k = key_index();
			bytes(0);
			bytes(1);
			bytes(2);
			if (field_len[1] != SECRET_LEN || field_len[2] != PUBLIC_LEN) {
				stop("a key of the wrong length");
			}
			memcpy(secrets[k], field[1], SECRET_LEN);
			memcpy(publics[k], field[2], PUBLIC_LEN);
			have[k] = 1;
			count[0]++;
			if (!fndsa_keygen_seeded_temp(LOGN, field[0], field_len[0],
				secret, public, tmp, sizeof tmp)
				|| memcmp(secret, secrets[k], SECRET_LEN) != 0
				|| memcmp(public, publics[k], PUBLIC_LEN) != 0)
			{
				differ[0]++;
				fprintf(stderr, "cross-check: line %lu:"
					" a different key pair from the same seed\n",
					line_number);
			}
		} else if (strcmp(kind, "sig") == 0) {
			uint8_t sig[SIG_LEN];
			size_t len;

			k = key_index();
			bytes(0);
			bytes(1);
			bytes(2);
			if (!have[k]) {
				stop("a key that was not given");
			}
			count[1]++;
			len = fndsa_sign_seeded_temp(secrets[k], SECRET_LEN,
				NULL, 0, FNDSA_HASH_ID_RAW, field[1], field_len[1],
				field[0], field_len[0],
				sig, sizeof sig, tmp, sizeof tmp);
			if (len != field_len[2]
				|| memcmp(sig, field[2], len) != 0)
			{
				differ[1]++;
				fprintf(stderr, "cross-check: line %lu:"
					" a different signature from the same seed\n",
					line_number);
			}
		} else if (strcmp(kind, "ver") == 0) {
			unsigned long expected;
			int accepted;

			k = key_index();
			bytes(0);
			bytes(1);
			expected = number();
			if (!have[k]) {
				stop("a key that was not given");
			}
			count[2]++;
			accepted = fndsa_verify_temp(field[1], field_len[1],
				publics[k], PUBLIC_LEN,
				NULL, 0, FNDSA_HASH_ID_RAW, field[0], field_len[0],
				tmp, sizeof tmp);
			if ((unsigned long)(accepted != 0) != expected) {
				differ[2]++;
				fprintf(stderr, "cross-check: line %lu:"
					" Stein %s this signature, the C code %s it\n",
					line_number,
					expected ? "accepts" : "rejects",
					accepted ? "accepts" : "rejects");
			}
		} else {
			stop("an unknown kind of line");
		}
	}
	if (ferror(f)) {
		perror(argv[1]);
		return 2;
	}
	fclose(f);

	for (i = 0; i < 3; i++) {
		printf("cross-check: %lu %s, %lu differ\n",
			count[i], names[i], differ[i]);
		/* A check that compared nothing has not passed. */
		if (count[i] == 0 || differ[i] != 0) {
			failed = 1;
		}
	}
	return failed;
}
