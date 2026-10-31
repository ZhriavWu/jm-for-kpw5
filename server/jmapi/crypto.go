package jmapi

import (
	"crypto/aes"
	"crypto/md5"
	"encoding/base64"
	"encoding/hex"
	"fmt"
)

func md5Hex(s string) string {
	h := md5.Sum([]byte(s))
	return hex.EncodeToString(h[:])
}

func tokenAndTokenParam(ts string, version string, secret string) (string, string) {
	if version == "" {
		version = DefaultAppVersion
	}
	if secret == "" {
		secret = AppTokenSecret
	}
	return md5Hex(ts + secret), fmt.Sprintf("%s,%s", ts, version)
}

func decodeRespData(data string, ts string, secret string) ([]byte, error) {
	if secret == "" {
		secret = AppDataSecret
	}
	raw, err := base64.StdEncoding.DecodeString(data)
	if err != nil {
		return nil, err
	}

	key := []byte(md5Hex(ts + secret))
	block, err := aes.NewCipher(key)
	if err != nil {
		return nil, err
	}
	if len(raw)%aes.BlockSize != 0 {
		return nil, fmt.Errorf("encrypted payload length %d is not multiple of block size", len(raw))
	}

	out := make([]byte, len(raw))
	for i := 0; i < len(raw); i += aes.BlockSize {
		block.Decrypt(out[i:i+aes.BlockSize], raw[i:i+aes.BlockSize])
	}

	if len(out) == 0 {
		return nil, fmt.Errorf("empty decrypted payload")
	}
	pad := int(out[len(out)-1])
	if pad <= 0 || pad > aes.BlockSize || pad > len(out) {
		return nil, fmt.Errorf("invalid pkcs7 padding")
	}
	return out[:len(out)-pad], nil
}
