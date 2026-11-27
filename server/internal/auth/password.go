package auth

import (
	"crypto/rand"
	"crypto/subtle"
	"encoding/base64"
	"errors"
	"fmt"
	"runtime"
	"strings"
	"sync/atomic"

	"golang.org/x/crypto/argon2"
)

// argon2id parameters (§5.5): m from REQHARBOR_ARGON2_MEMORY_KIB, t=3, p=2, 16-byte salt, 32-byte key.
const (
	argonTime    = 3
	argonThreads = 2
	saltBytes    = 16
	keyBytes     = 32
)

// Hasher hashes and verifies passwords as PHC strings, with a bounded number of hashes at once so
// a login burst cannot exhaust the memory of a small server.
type Hasher struct {
	memoryKiB uint32
	sem       chan struct{}
	dummy     string
	// Verifies counts argon2 verifications, real and dummy (tests check that known and unknown
	// emails run argon2 on the same paths).
	Verifies atomic.Int64
}

// NewHasher returns a hasher using memoryKiB per hash.
func NewHasher(memoryKiB int64) *Hasher {
	h := &Hasher{memoryKiB: uint32(memoryKiB), sem: make(chan struct{}, max(1, min(4, runtime.NumCPU()/2)))}
	var pw [24]byte
	_, _ = rand.Read(pw[:])
	dummy, err := h.Hash(base64.RawStdEncoding.EncodeToString(pw[:]))
	if err != nil {
		panic(err)
	}
	h.dummy = dummy
	return h
}

func (h *Hasher) key(password string, salt []byte, m uint32, t uint32, p uint8) []byte {
	h.sem <- struct{}{}
	defer func() { <-h.sem }()
	return argon2.IDKey([]byte(password), salt, t, m, p, keyBytes)
}

// Hash returns `$argon2id$v=19$m=…,t=3,p=2$<salt>$<key>`.
func (h *Hasher) Hash(password string) (string, error) {
	salt := make([]byte, saltBytes)
	if _, err := rand.Read(salt); err != nil {
		return "", err
	}
	k := h.key(password, salt, h.memoryKiB, argonTime, argonThreads)
	return fmt.Sprintf("$argon2id$v=%d$m=%d,t=%d,p=%d$%s$%s", argon2.Version, h.memoryKiB, argonTime, argonThreads,
		base64.RawStdEncoding.EncodeToString(salt), base64.RawStdEncoding.EncodeToString(k)), nil
}

type phcParams struct {
	m, t uint32
	p    uint8
	salt []byte
	key  []byte
}

var errPHC = errors.New("auth: malformed argon2id PHC string")

func parsePHC(s string) (*phcParams, error) {
	parts := strings.Split(s, "$")
	if len(parts) != 6 || parts[0] != "" || parts[1] != "argon2id" || parts[2] != fmt.Sprintf("v=%d", argon2.Version) {
		return nil, errPHC
	}
	var pp phcParams
	var p uint32
	if _, err := fmt.Sscanf(parts[3], "m=%d,t=%d,p=%d", &pp.m, &pp.t, &p); err != nil || pp.m == 0 || pp.t == 0 || p == 0 || p > 255 {
		return nil, errPHC
	}
	pp.p = uint8(p)
	var err error
	if pp.salt, err = base64.RawStdEncoding.DecodeString(parts[4]); err != nil || len(pp.salt) == 0 {
		return nil, errPHC
	}
	if pp.key, err = base64.RawStdEncoding.DecodeString(parts[5]); err != nil || len(pp.key) == 0 {
		return nil, errPHC
	}
	return &pp, nil
}

// Verify checks password against phc. rehash is true when phc uses other parameters than this
// hasher, so the caller stores a fresh hash after a successful login.
func (h *Hasher) Verify(phc, password string) (ok, rehash bool) {
	h.Verifies.Add(1)
	pp, err := parsePHC(phc)
	if err != nil {
		// Spend the same time as a real verification.
		dp, _ := parsePHC(h.dummy)
		h.key(password, dp.salt, dp.m, dp.t, dp.p)
		return false, false
	}
	k := h.key(password, pp.salt, pp.m, pp.t, pp.p)
	if subtle.ConstantTimeCompare(k, pp.key) != 1 {
		return false, false
	}
	return true, pp.m != h.memoryKiB || pp.t != argonTime || pp.p != argonThreads || len(pp.salt) != saltBytes || len(pp.key) != keyBytes
}

// VerifyDummy runs a verification against a fixed dummy hash, for an unknown email, so timing
// does not reveal whether the account exists. It always fails.
func (h *Hasher) VerifyDummy(password string) {
	h.Verify(h.dummy, password)
}
