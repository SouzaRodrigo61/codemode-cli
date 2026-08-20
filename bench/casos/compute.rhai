// Laço puro de VM: mede o custo por operação do interpretador, inclusive o que
// as guardas de timeout cobram a cada operação.
let n = 0;
for i in 0..200000 { n += i % 7; }
