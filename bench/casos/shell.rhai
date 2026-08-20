// Comando de shell: mede o piso de spawn e o imposto do sh.
for i in 0..10 { run_shell("true"); }
for i in 0..5 { run_shell("sh -c 'true'"); }
