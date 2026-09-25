# Shared by every interactive Bash, including the graphical terminal.
[[ $- == *i* ]] || return
export PATH=/bin:/sbin:/usr/bin:/usr/sbin
export HISTFILE="$HOME/.bash_history" HISTSIZE=5000 HISTFILESIZE=10000
HISTCONTROL=ignoreboth:erasedups
shopt -s histappend checkwinsize cmdhist
# Append this session and read commands from other terminals on every prompt.
PROMPT_COMMAND='history -a; history -n'
PS1='\[\e[32m\]\u@\h\[\e[0m\]:\w\$ '
alias ls='ls --color=auto'
alias ll='ls -lah'
alias la='ls -A'
# Readline supplies filename/command completion and Ctrl+R history search.
complete -d cd
complete -c command exec
