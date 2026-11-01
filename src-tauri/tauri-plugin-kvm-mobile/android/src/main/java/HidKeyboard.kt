package local.kvm.mobile

object HidKeyboard {
    fun character(char: Char): Pair<Int, Int>? {
        if (char in 'a'..'z' || char in 'A'..'Z') return (char.lowercaseChar() - 'a' + 4) to (if (char.isUpperCase()) 2 else 0)
        val shifted = "!@#$%^&*()"
        if (char in shifted) return (30 + shifted.indexOf(char)) to 2
        val punctuation = mapOf('_' to 45, '+' to 46, '{' to 47, '}' to 48, '|' to 49, ':' to 51, '"' to 52, '~' to 53, '<' to 54, '>' to 55, '?' to 56)
        punctuation[char]?.let { return it to 2 }
        val plain = "1234567890\n\u001b\b\t -=[]\\#;'`,./"
        val index = plain.indexOf(char)
        return if (index >= 0) (30 + index) to 0 else null
    }
}
