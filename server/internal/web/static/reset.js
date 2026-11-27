(function(){var m=/(?:^#|&)t=([^&]*)/.exec(location.hash);var f=document.getElementById("t");if(m&&f){f.value=decodeURIComponent(m[1]);history.replaceState(null,"",location.pathname)}})();
